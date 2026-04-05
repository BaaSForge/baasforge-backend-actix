//! TypeScript type-checking for LLM-generated unit code.
//!
//! Build the `unit_technical_reviewer` binary with
//! `--features unit_technical_reviewer` to enable the real checker.
//!
//! Runtime requirement: `tsc` (TypeScript compiler) must be in PATH, or `npx`
//! must be available so that `npx --yes tsc` can download and run it.
//!
//! Supported node:* specifiers (any other node:* import is rejected):
//!   node:http, node:querystring, node:stream/consumers, node:url,
//!   node:crypto, node:buffer, node:util, node:timers/promises
//!
//! @units/* imports are resolved to a stub that exports `type Sample = {}`.

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

/// Stub declaration content for @units/* — all @units paths resolve here.
const UNITS_STUB_DTS: &str = "export type Sample = {};\n";

/// Global ambient declaration — allows any import from @units/* in user code.
const GLOBAL_DTS: &str = "declare module \"@units/*\" {\n    export type Sample = {};\n}\n";

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
pub async fn run_typescript_check(code: &str) -> Result<(), String> {
    #[cfg(feature = "unit_technical_reviewer")]
    return run_impl(code).await;

    #[cfg(not(feature = "unit_technical_reviewer"))]
    {
        let _ = code;
        Ok(())
    }
}

#[cfg(feature = "unit_technical_reviewer")]
async fn run_impl(code: &str) -> Result<(), String> {
    use oxc_allocator::Allocator;
    use oxc_ast::ast::Statement;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    // ── Step 1: Parse with oxc ───────────────────────────────────────────────
    // Catches syntax errors fast, in-process, before spawning any subprocess.
    let allocator = Allocator::default();
    let source_type = SourceType::ts();
    let ret = Parser::new(&allocator, code, source_type).parse();

    if !ret.errors.is_empty() {
        let msgs: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();
        return Err(format!("Syntax error(s):\n{}", msgs.join("\n")));
    }

    // ── Step 2: Validate node:* import specifiers ────────────────────────────
    // Any node:* import that is not in the supported list is rejected before
    // running tsc, so the error message is clear and immediate.
    for stmt in &ret.program.body {
        if let Statement::ImportDeclaration(decl) = stmt {
            let spec = decl.source.value.as_str();
            if spec.starts_with("node:") && !SUPPORTED_NODE_SPECIFIERS.contains(&spec) {
                return Err(format!(
                    "Unsupported node:* import: \"{spec}\". \
                     Only these are supported: {}",
                    SUPPORTED_NODE_SPECIFIERS.join(", ")
                ));
            }
        }
    }

    // ── Step 3: Write virtual filesystem to a temp directory ─────────────────
    let tmp = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let tmp_path = tmp.path();

    // main.ts — the LLM's code, unchanged
    std::fs::write(tmp_path.join("main.ts"), code)
        .map_err(|e| format!("Failed to write main.ts: {e}"))?;

    // global.d.ts — ambient wildcard module for @units/*
    std::fs::write(tmp_path.join("global.d.ts"), GLOBAL_DTS)
        .map_err(|e| format!("Failed to write global.d.ts: {e}"))?;

    // units-stub.d.ts — stub target for the @units/* paths mapping
    std::fs::write(tmp_path.join("units-stub.d.ts"), UNITS_STUB_DTS)
        .map_err(|e| format!("Failed to write units-stub.d.ts: {e}"))?;

    // node_modules/@types/node — all embedded declaration files
    let types_dir = tmp_path.join("node_modules").join("@types").join("node");
    for (rel_path, content) in node_types() {
        let dest = types_dir.join(rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create dir: {e}"))?;
        }
        std::fs::write(&dest, content)
            .map_err(|e| format!("Failed to write @types/node/{rel_path}: {e}"))?;
    }
    // package.json so TypeScript can locate the @types/node package
    std::fs::write(
        types_dir.join("package.json"),
        r#"{"name":"@types/node","version":"22.0.0","main":"","types":"index.d.ts"}"#,
    )
    .map_err(|e| format!("Failed to write @types/node/package.json: {e}"))?;

    // tsconfig.json
    let tsconfig = serde_json::json!({
        "compilerOptions": {
            "target": "ESNext",
            "module": "NodeNext",
            "moduleResolution": "NodeNext",
            "strict": true,
            "noEmit": true,
            "skipLibCheck": false,
            "paths": {
                "@units/*": ["./units-stub.d.ts"]
            }
        },
        "include": ["main.ts", "global.d.ts"]
    });
    std::fs::write(
        tmp_path.join("tsconfig.json"),
        serde_json::to_string_pretty(&tsconfig).unwrap(),
    )
    .map_err(|e| format!("Failed to write tsconfig.json: {e}"))?;

    // ── Step 4: Run the TypeScript compiler ──────────────────────────────────
    let output = spawn_tsc(tmp_path).await?;

    if output.status.success() {
        return Ok(());
    }

    // tsc writes diagnostics to stdout; strip the temp-dir prefix for cleaner output
    let raw = String::from_utf8_lossy(&output.stdout);
    let tmp_prefix = tmp_path.to_string_lossy();
    let clean = raw.replace(tmp_prefix.as_ref(), "").replace('\\', "/");
    Err(format!("TypeScript type errors:\n{}", clean.trim()))
}

/// Spawns `tsc --noEmit` in `tmp_path`, falling back to `npx --yes tsc` if
/// `tsc` is not found in PATH.
#[cfg(feature = "unit_technical_reviewer")]
async fn spawn_tsc(tmp_path: &std::path::Path) -> Result<std::process::Output, String> {
    use tokio::process::Command;

    let tsc_result = Command::new("tsc")
        .arg("--noEmit")
        .current_dir(tmp_path)
        .output()
        .await;

    match tsc_result {
        Ok(out) => return Ok(out),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Failed to run tsc: {e}")),
    }

    // tsc not in PATH — try via npx (which can download it on first use)
    Command::new("npx")
        .args(["--yes", "tsc", "--noEmit"])
        .current_dir(tmp_path)
        .output()
        .await
        .map_err(|e| {
            format!(
                "TypeScript compiler not found. \
                 Install with `npm install -g typescript`. Error: {e}"
            )
        })
}
