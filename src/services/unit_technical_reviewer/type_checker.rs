//! TypeScript type-checking for LLM-generated unit code.
//!
//! Uses **tsgo** (Microsoft's Go-based TypeScript native compiler) via its
//! `--api --async` IPC server with a fully **virtual in-memory filesystem**.
//!
//! * No files written to disk — all content served from memory via callbacks.
//! * No Node.js runtime — tsgo is a native Go binary (no V8, no npm).
//! * No temp directories — the virtual root [`VIRTUAL_ROOT`] lives only in RAM.
//!
//! ## Protocol
//!
//! 1. Spawn `tsgo --api --async --cwd <VIRTUAL_ROOT> --callbacks=readFile,...`
//! 2. JSON-RPC 2.0 over stdin/stdout framed with `Content-Length:` headers
//!    (same base protocol as LSP).
//! 3. tsgo calls back for every filesystem read; we respond from our in-memory
//!    VFS ([`build_vfs`]).
//! 4. Request sequence:
//!    `initialize → updateSnapshot → getDefaultProjectForFile →
//!     getSemanticDiagnostics → release`
//!
//! ## Install tsgo
//!
//! ```sh
//! cd tsgo && npm install   # one-time setup — installs into tsgo/node_modules/
//! ```
//! The binary is resolved in-place from `tsgo/node_modules/<platform>/lib/tsgo`.

#[cfg(feature = "unit_technical_reviewer")]
const SUPPORTED_NODE_SPECIFIERS: &[&str] = &[
    "node:http",
    "node:querystring",
    "node:stream/consumers",
    "node:url",
    "node:crypto",
    "node:buffer",
    "node:util",
    "node:timers/promises",
];

#[cfg(feature = "unit_technical_reviewer")]
const UNITS_STUB_DTS: &str = "export type Sample = {};\n";
#[cfg(feature = "unit_technical_reviewer")]
const GLOBAL_DTS: &str = "declare module \"@units/*\" {\n    export type Sample = {};\n}\n";

/// Virtual working-directory passed to `tsgo --cwd`.
/// Must be an absolute path that does NOT exist on the real filesystem.
#[cfg(feature = "unit_technical_reviewer")]
const VIRTUAL_ROOT: &str = "/tsgo-vcheck-baal";

/// Timeout for a single tsgo type-check invocation.
#[cfg(feature = "unit_technical_reviewer")]
const TSGO_TIMEOUT_SECS: u64 = 30;

// All @types/node declaration files embedded at compile time via build.rs.
// Only compiled when the feature is active to avoid bloating other binaries.
#[cfg(feature = "unit_technical_reviewer")]
include!(concat!(env!("OUT_DIR"), "/node_type_files.rs"));

#[cfg(feature = "unit_technical_reviewer")]
static NODE_TYPES: std::sync::OnceLock<std::collections::HashMap<&'static str, &'static str>> =
    std::sync::OnceLock::new();

#[cfg(feature = "unit_technical_reviewer")]
fn node_types() -> &'static std::collections::HashMap<&'static str, &'static str> {
    NODE_TYPES.get_or_init(|| NODE_TYPE_FILE_DATA.iter().copied().collect())
}

/// Type-checks TypeScript `code` written by the LLM.
///
/// Returns `Ok(())` on success, `Err(diagnostic)` on any failure
/// (syntax error, unsupported import, or TypeScript type error).
///
/// Without the `unit_technical_reviewer` feature this returns `Err` so that a
/// forgotten feature flag is caught at runtime rather than silently skipped.
pub async fn run_typescript_check(code: &str) -> Result<(), String> {
    #[cfg(feature = "unit_technical_reviewer")]
    return run_impl(code).await;

    #[cfg(not(feature = "unit_technical_reviewer"))]
    {
        let _ = code;
        Err(
            "TypeScript type checking is disabled. \
             Build unit_technical_reviewer with --features unit_technical_reviewer."
                .to_string(),
        )
    }
}

#[cfg(feature = "unit_technical_reviewer")]
async fn run_impl(code: &str) -> Result<(), String> {
    use oxc_allocator::Allocator;
    use oxc_ast::ast::Statement;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    // ── 1. Fast in-process syntax check (OXC) ───────────────────────────────
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, code, SourceType::ts()).parse();
    if !ret.errors.is_empty() {
        let msgs: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();
        return Err(format!("Syntax error(s):\n{}", msgs.join("\n")));
    }

    // ── 2. node:* import allowlist ───────────────────────────────────────────
    for stmt in &ret.program.body {
        if let Statement::ImportDeclaration(decl) = stmt {
            let spec = decl.source.value.as_str();
            if spec.starts_with("node:") && !SUPPORTED_NODE_SPECIFIERS.contains(&spec) {
                return Err(format!(
                    "Unsupported node:* import: \"{spec}\". \
                     Supported: {}",
                    SUPPORTED_NODE_SPECIFIERS.join(", ")
                ));
            }
        }
    }

    // ── 3. Build virtual filesystem ──────────────────────────────────────────
    let vfs = build_vfs(code);

    // ── 4. Type-check via tsgo IPC (with timeout) ────────────────────────────
    let diags = tokio::time::timeout(
        std::time::Duration::from_secs(TSGO_TIMEOUT_SECS),
        TsgoIpcClient::run_check(vfs),
    )
    .await
    .map_err(|_| {
        format!(
            "TypeScript check timed out after {TSGO_TIMEOUT_SECS}s. \
             Ensure tsgo is installed and responding."
        )
    })??;

    if diags.is_empty() {
        return Ok(());
    }

    let msgs: Vec<String> = diags
        .into_iter()
        .map(|d| {
            let file = d
                .file
                .strip_prefix(VIRTUAL_ROOT)
                .unwrap_or(&d.file)
                .trim_start_matches('/')
                .to_string();
            format!(
                "{file}:{line}:{col} — TS{code}: {msg}",
                line = d.line + 1,
                col = d.character + 1,
                code = d.code,
                msg = d.message_text,
            )
        })
        .collect();

    Err(format!("TypeScript type errors:\n{}", msgs.join("\n")))
}

// ── Virtual filesystem ────────────────────────────────────────────────────────

/// Build the in-memory VFS that tsgo will query via filesystem callbacks.
///
/// Layout under [`VIRTUAL_ROOT`]:
/// ```text
/// /tsgo-vcheck-baal/
///   tsconfig.json
///   main.ts               ← the LLM code under test
///   global.d.ts           ← ambient @units/* wildcard declaration
///   units-stub.d.ts       ← stub target for @units/* paths mapping
///   node_modules/
///     @types/node/
///       package.json
///       **/*.d.ts         ← all files embedded at build time via build.rs
/// ```
#[cfg(feature = "unit_technical_reviewer")]
fn build_vfs(code: &str) -> std::collections::HashMap<String, String> {
    let mut vfs = std::collections::HashMap::new();

    let tsconfig = serde_json::json!({
        "compilerOptions": {
            "target": "ESNext",
            "module": "NodeNext",
            "moduleResolution": "NodeNext",
            "strict": true,
            "noImplicitAny": true,
            "strictNullChecks": true,
            "noEmit": true,
            "skipLibCheck": false,
            // baseUrl is required when using paths in tsgo; TS5+ allows omitting it
            // but being explicit avoids any ambiguity in tsgo's resolver.
            "baseUrl": ".",
            "paths": { "@units/*": ["./units-stub.d.ts"] }
        },
        // Explicitly disable automatic type acquisition (top-level tsconfig field).
        // tsgo's API mode does not initialise sessionOptions, so accessing
        // sessionOptions.TypingsLocation at project/project.go:362 panics when
        // typeAcquisition is enabled.  An explicit false prevents that branch.
        "typeAcquisition": { "enable": false },
        "include": ["main.ts", "global.d.ts", "units-stub.d.ts"]
    });

    vfs.insert(
        format!("{VIRTUAL_ROOT}/tsconfig.json"),
        serde_json::to_string_pretty(&tsconfig).unwrap(),
    );
    vfs.insert(format!("{VIRTUAL_ROOT}/main.ts"), code.to_string());
    vfs.insert(format!("{VIRTUAL_ROOT}/global.d.ts"), GLOBAL_DTS.to_string());
    vfs.insert(format!("{VIRTUAL_ROOT}/units-stub.d.ts"), UNITS_STUB_DTS.to_string());

    // Embed all @types/node declaration files from build-time data.
    let types_base = format!("{VIRTUAL_ROOT}/node_modules/@types/node");
    for (rel, content) in node_types() {
        vfs.insert(format!("{types_base}/{rel}"), content.to_string());
    }
    vfs.insert(
        format!("{types_base}/package.json"),
        r#"{"name":"@types/node","version":"22.0.0","main":"","types":"index.d.ts"}"#.to_string(),
    );

    vfs
}

// ── Diagnostic type ───────────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
#[derive(Debug)]
struct TsDiagnostic {
    file: String,
    line: u32,
    character: u32,
    code: u32,
    message_text: String,
}

// ── tsgo IPC client ───────────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
struct TsgoIpcClient {
    child: tokio::process::Child,
    writer: tokio::io::BufWriter<tokio::process::ChildStdin>,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    stderr: tokio::io::BufReader<tokio::process::ChildStderr>,
    next_id: u64,
    vfs: std::collections::HashMap<String, String>,
}

#[cfg(feature = "unit_technical_reviewer")]
impl TsgoIpcClient {
    /// Spawn tsgo, run the full check lifecycle, and return diagnostics.
    async fn run_check(
        vfs: std::collections::HashMap<String, String>,
    ) -> Result<Vec<TsDiagnostic>, String> {
        let mut c = Self::spawn(vfs).await?;
        c.do_check().await
    }

    async fn do_check(&mut self) -> Result<Vec<TsDiagnostic>, String> {
        let tsconfig = format!("{VIRTUAL_ROOT}/tsconfig.json");
        let main_ts = format!("{VIRTUAL_ROOT}/main.ts");

        // ── initialize ───────────────────────────────────────────────────────
        self.call("initialize", serde_json::json!({})).await?;

        // ── create snapshot (opens the virtual tsconfig project) ─────────────
        let snap_resp = self
            .call(
                "updateSnapshot",
                serde_json::json!({
                    "openProject": tsconfig,
                    "fileChanges": {
                        "invalidateAll": false,
                        "changed":  [],
                        "created":  [],
                        "deleted":  []
                    }
                }),
            )
            .await?;

        let snapshot = snap_resp
            .get("snapshot")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                format!("updateSnapshot: missing 'snapshot' field. Response: {snap_resp}")
            })?
            .to_string();

        // ── resolve the project that owns main.ts ────────────────────────────
        let proj_resp = self
            .call(
                "getDefaultProjectForFile",
                serde_json::json!({
                    "snapshot": snapshot,
                    "file": main_ts
                }),
            )
            .await?;

        let project = proj_resp
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                format!(
                    "getDefaultProjectForFile returned no project for {main_ts}. \
                     Response: {proj_resp}"
                )
            })?
            .to_string();

        // ── collect semantic diagnostics for main.ts only ────────────────────
        let diags_json = self
            .call(
                "getSemanticDiagnostics",
                serde_json::json!({
                    "snapshot": snapshot,
                    "project":  project,
                    "file":     main_ts
                }),
            )
            .await?;

        // ── release snapshot (best-effort) ───────────────────────────────────
        let _ = self
            .call("release", serde_json::json!({ "handle": snapshot }))
            .await;

        // Capture vfs reference before self is consumed
        let main_ts_clone = main_ts.clone();
        Ok(parse_diagnostics(&diags_json, &self.vfs, &main_ts_clone))
    }

    /// Spawn the tsgo binary in IPC mode with virtual-FS callbacks enabled.
    async fn spawn(vfs: std::collections::HashMap<String, String>) -> Result<Self, String> {
        use tokio::io::{BufReader, BufWriter};

        let tsgo_bin = find_tsgo_bin()?;
        let args = [
            "--api",
            "--async",
            "--cwd",
            VIRTUAL_ROOT,
            "--callbacks=readFile,fileExists,directoryExists,getAccessibleEntries",
        ];
        tracing::debug!(bin = %tsgo_bin, ?args, "spawning tsgo");

        let mut child = tokio::process::Command::new(&tsgo_bin)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn tsgo ('{tsgo_bin}'): {e}"))?;

        let writer = BufWriter::new(child.stdin.take().expect("stdin was piped"));
        let reader = BufReader::new(child.stdout.take().expect("stdout was piped"));
        let stderr = BufReader::new(child.stderr.take().expect("stderr was piped"));

        Ok(Self { child, writer, reader, stderr, next_id: 1, vfs })
    }

    /// Send a JSON-RPC request and return its `result`, handling any interleaved
    /// filesystem callback requests from tsgo synchronously in the same loop.
    ///
    /// tsgo's async IPC is strictly ordered from our perspective: it sends
    /// callbacks *before* responding to our request, so a simple read loop
    /// handles both without any additional concurrency.
    async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id.to_string();
        self.next_id += 1;

        tracing::debug!(id = %id, method, "→ tsgo request");

        self.write_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id":      id,
            "method":  method,
            "params":  params
        }))
        .await?;

        loop {
            let msg = self.read_message().await?;

            // Callback from tsgo: has a "method" field that WE did not initiate.
            if let Some(cb_method) = msg.get("method").and_then(|v| v.as_str()) {
                let cb_id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
                let cb_params =
                    msg.get("params").cloned().unwrap_or(serde_json::Value::Null);

                let path = cb_params
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                tracing::debug!(cb_method, path, "← tsgo fs callback");

                let result = self.handle_fs_callback(cb_method, &cb_params);
                self.write_message(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id":      cb_id,
                    "result":  result
                }))
                .await?;
                continue;
            }

            // Response to our request (id matches).
            if msg.get("id").and_then(|v| v.as_str()) == Some(id.as_str()) {
                tracing::debug!(id = %id, method, "← tsgo response");
                return match msg.get("error") {
                    Some(err) => Err(format!(
                        "tsgo '{method}': {}",
                        err.get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error")
                    )),
                    None => Ok(msg
                        .get("result")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null)),
                };
            }

            tracing::debug!(msg = %msg, "← tsgo unexpected message (ignored)");
        }
    }

    /// Respond to a virtual-filesystem callback from tsgo.
    ///
    /// tsgo sends callbacks as JSON-RPC requests with params as a JSON **array**:
    /// `{"method": "readFile", "params": ["/tsgo-vcheck-baal/main.ts"]}`
    fn handle_fs_callback(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        // All callback params are positional arrays; first element is the path.
        let path = params
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match method {
            "readFile" => match self.vfs.get(path) {
                Some(content) => serde_json::json!({ "content": content }),
                // null → tsgo falls back to the real filesystem (used for TypeScript
                // lib files that live next to the binary on the real FS).
                None => serde_json::Value::Null,
            },

            // Must return JSON boolean true/false (not strings).
            // null → tsgo falls back to the real filesystem.
            "fileExists" => {
                if self.vfs.contains_key(path) {
                    serde_json::Value::Bool(true)
                } else {
                    serde_json::Value::Null // fall back to real FS
                }
            }

            "directoryExists" => {
                let prefix = dir_prefix(path);
                if self.vfs.keys().any(|k| k.starts_with(&prefix)) {
                    serde_json::Value::Bool(true)
                } else {
                    serde_json::Value::Null // fall back to real FS
                }
            }

            "getAccessibleEntries" => {
                let prefix = dir_prefix(path);
                let mut files: Vec<String> = Vec::new();
                let mut subdirs: std::collections::BTreeSet<String> = Default::default();

                for key in self.vfs.keys() {
                    if let Some(rel) = key.strip_prefix(&prefix) {
                        match rel.find('/') {
                            Some(slash) => {
                                subdirs.insert(rel[..slash].to_string());
                            }
                            None => files.push(rel.to_string()),
                        }
                    }
                }
                if files.is_empty() && subdirs.is_empty() {
                    // Directory not in our VFS — fall back to real FS.
                    serde_json::Value::Null
                } else {
                    serde_json::json!({
                        "files":       files,
                        "directories": subdirs.into_iter().collect::<Vec<_>>()
                    })
                }
            }

            _ => serde_json::Value::Null,
        }
    }

    /// Write one JSON-RPC message with a `Content-Length:` header.
    async fn write_message(&mut self, msg: &serde_json::Value) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;

        let body = msg.to_string();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer
            .write_all(header.as_bytes())
            .await
            .map_err(|e| format!("tsgo write header: {e}"))?;
        self.writer
            .write_all(body.as_bytes())
            .await
            .map_err(|e| format!("tsgo write body: {e}"))?;
        self.writer
            .flush()
            .await
            .map_err(|e| format!("tsgo flush: {e}"))?;
        Ok(())
    }

    /// Read one `Content-Length:`-framed JSON-RPC message from tsgo's stdout.
    async fn read_message(&mut self) -> Result<serde_json::Value, String> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};

        // Consume headers until the blank line that precedes the body.
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|e| format!("tsgo read header line: {e}"))?;
            if n == 0 {
                return Err(self.closed_unexpectedly_error().await);
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break; // blank line → body follows
            }
            if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
                content_length = Some(
                    val.trim()
                        .parse::<usize>()
                        .map_err(|_| format!("tsgo bad Content-Length: {val}"))?,
                );
            }
        }

        let len =
            content_length.ok_or_else(|| "tsgo: Content-Length header missing".to_string())?;
        if len == 0 {
            return Err("tsgo: Content-Length is zero".to_string());
        }

        let mut body = vec![0u8; len];
        self.reader
            .read_exact(&mut body)
            .await
            .map_err(|e| format!("tsgo read body ({len} B): {e}"))?;

        serde_json::from_slice(&body).map_err(|e| {
            format!(
                "tsgo JSON parse: {e} — raw: {}",
                String::from_utf8_lossy(&body)
            )
        })
    }

    /// Build a rich error for the "tsgo exited with stdout closed" case.
    /// Collects stderr output and exit status so the caller can diagnose why
    /// the process died (wrong binary, unsupported flags, crash, etc.).
    async fn closed_unexpectedly_error(&mut self) -> String {
        use tokio::io::AsyncReadExt;

        // Drain stderr (process has exited so this completes immediately).
        let mut stderr_text = String::new();
        let _ = self.stderr.read_to_string(&mut stderr_text).await;

        // Get exit status without blocking.
        let status = self
            .child
            .try_wait()
            .ok()
            .flatten()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "still running (or unknown)".to_string());

        let mut msg = format!(
            "tsgo exited unexpectedly (status: {status}).\n\
             Binary: {bin}\n\
             Hint: binary may be missing or misplaced — re-run `cd tsgo && npm install`",
            bin = find_tsgo_bin().unwrap_or_else(|_| "<unknown>".into()),
        );
        if !stderr_text.trim().is_empty() {
            msg.push_str(&format!("\nstderr:\n{}", stderr_text.trim()));
        }
        tracing::error!(%msg, "tsgo process closed stdout");
        msg
    }
}

#[cfg(feature = "unit_technical_reviewer")]
impl Drop for TsgoIpcClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Locate the tsgo native binary inside `tsgo/node_modules/`.
///
/// The binary lives in-place at its npm-installed path so that sibling files
/// (e.g. `lib.d.ts`) are available next to it — copying it elsewhere breaks
/// the non-embedded build variant.
///
/// * **Production** — the binary is deployed at `ROOT/unit_technical_reviewer`,
///   so `exe.parent()` is `ROOT/` and `tsgo/node_modules/…/tsgo` is found there.
/// * **Development** — `cargo run` is invoked from `ROOT/`, so cwd is `ROOT/`
///   and `tsgo/node_modules/…/tsgo` is found relative to it.
///
/// Run `cd tsgo && npm install` once from the repo root to populate `tsgo/node_modules/`.
#[cfg(feature = "unit_technical_reviewer")]
fn find_tsgo_bin() -> Result<String, String> {
    let rel = tsgo_bin_rel_path()?;

    // Production: exe lives at ROOT/ → parent is the project root.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe.parent() {
            let p = root.join(&rel);
            if p.exists() {
                tracing::debug!(path = %p.display(), "tsgo resolved via exe parent");
                return Ok(p.to_string_lossy().into_owned());
            }
            tracing::debug!(checked = %p.display(), "tsgo not found via exe parent");
        }
    }

    // Development: cwd is the project root when invoking `cargo run`.
    let p = std::path::PathBuf::from(&rel);
    if p.exists() {
        tracing::debug!(path = %p.display(), "tsgo resolved via cwd");
        return Ok(p.to_string_lossy().into_owned());
    }
    tracing::debug!(checked = %p.display(), "tsgo not found via cwd");

    Err(format!(
        "tsgo binary not found at {rel} relative to the project root. \
         Run `cd tsgo && npm install` from the repo root."
    ))
}

/// Returns the relative path to the tsgo native binary inside `tsgo/node_modules/`
/// for the current platform.
#[cfg(feature = "unit_technical_reviewer")]
fn tsgo_bin_rel_path() -> Result<String, String> {
    let pkg = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "@typescript/native-preview-darwin-arm64",
        ("macos", "x86_64") => "@typescript/native-preview-darwin-x64",
        ("linux", "x86_64") => "@typescript/native-preview-linux-x64",
        ("linux", "aarch64") => "@typescript/native-preview-linux-arm64",
        (os, arch) => {
            return Err(format!(
                "Unsupported platform for tsgo: {os}-{arch}. \
                 Supported: macos-aarch64, macos-x86_64, linux-x86_64, linux-aarch64."
            ))
        }
    };
    Ok(format!("tsgo/node_modules/{pkg}/lib/tsgo"))
}

/// Ensure a path ends with `/` for use as a VFS prefix in `starts_with`.
#[cfg(feature = "unit_technical_reviewer")]
fn dir_prefix(path: &str) -> String {
    if path.ends_with('/') {
        path.to_string()
    } else {
        format!("{path}/")
    }
}

/// Parse the `getSemanticDiagnostics` JSON array into [`TsDiagnostic`]s.
///
/// tsgo may return either:
/// * `"line"` / `"character"` fields directly, or
/// * a `"start"` byte offset — in that case we compute line/col from the VFS.
#[cfg(feature = "unit_technical_reviewer")]
fn parse_diagnostics(
    value: &serde_json::Value,
    vfs: &std::collections::HashMap<String, String>,
    source_path: &str,
) -> Vec<TsDiagnostic> {
    let arr = match value.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .filter_map(|d| {
            // category: 1 = Error; skip Warnings (0), Suggestions (2), Messages (3).
            let category = d.get("category").and_then(|v| v.as_u64()).unwrap_or(1);
            if category != 1 {
                return None;
            }

            let file = d
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or(source_path)
                .to_string();
            let code = d.get("code").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let message_text = extract_message_text(d.get("messageText")?);

            let (line, character) = if d.get("line").is_some() {
                (
                    d.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    d.get("character")
                        .or_else(|| d.get("column"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                )
            } else if let Some(start) = d.get("start").and_then(|v| v.as_u64()) {
                let content = vfs.get(source_path).map(String::as_str).unwrap_or("");
                offset_to_line_col(content, start as usize)
            } else {
                (0, 0)
            };

            Some(TsDiagnostic { file, line, character, code, message_text })
        })
        .collect()
}

/// Recursively flatten a TypeScript `DiagnosticMessageChain` into a `String`.
#[cfg(feature = "unit_technical_reviewer")]
fn extract_message_text(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(obj) => {
            let head = obj
                .get("messageText")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            match obj.get("next") {
                Some(next) => format!("{head}: {}", extract_message_text(next)),
                None => head.to_string(),
            }
        }
        _ => "unknown error".to_string(),
    }
}

/// Convert a UTF-8 byte `offset` in `content` to 0-indexed `(line, col)`.
#[cfg(feature = "unit_technical_reviewer")]
fn offset_to_line_col(content: &str, offset: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, ch) in content.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}
