use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};

use crate::database::{DbPool, block_on_db_tokio};
use crate::models::projects::ProjectUnitResponse;
use crate::services::chat_worker::db::load_effective_units_for_chat_session;
use crate::services::unit_technical_reviewer::unit_vfs::{
    VIRTUAL_ROOT, build_unit_source_map, build_vfs, find_tsgo_bin,
};
use uuid::Uuid;

pub async fn run_typescript_check(
    pool: &DbPool,
    project_id: Uuid,
    session_id: Uuid,
    code: &str,
) -> Result<(), String> {
    // Pre-load all effective units so that `@units/*` imports resolve inside the VFS.
    let units = block_on_db_tokio(pool, move |conn| {
        load_effective_units_for_chat_session(conn, project_id, session_id)
    })
    .await
    .map_err(|e| format!("Failed to load units for type check: {}", e))?;

    let excerpt_sources = build_unit_source_map(code, &units);
    let vfs = build_vfs(code, &units);
    let mut client = TsgoIpcClient::spawn(vfs).await?;

    client.call("initialize", json!({})).await?;

    let tsconfig_path = format!("{VIRTUAL_ROOT}/tsconfig.json");
    let main_ts_path = format!("{VIRTUAL_ROOT}/main.ts");

    // 2. Snapshot Lifecycle
    let snap_resp = client
        .call(
            "updateSnapshot",
            json!({
                "openProject": tsconfig_path,
                "fileChanges": {
                    "invalidateAll": true,
                    "changed": [],
                    "created": [tsconfig_path, main_ts_path],
                    "deleted": []
                }
            }),
        )
        .await?;

    let snapshot = snap_resp["snapshot"]
        .as_str()
        .ok_or("No snapshot handle")?
        .to_string();

    // 3. Resolve Project
    let proj_resp = client
        .call(
            "getDefaultProjectForFile",
            json!({
                "snapshot": snapshot,
                "file": main_ts_path
            }),
        )
        .await?;

    let project = proj_resp["id"]
        .as_str()
        .ok_or_else(|| format!("No project id in response: {}", proj_resp))?;

    // 4. Get Diagnostics
    let diags = client
        .call(
            "getSemanticDiagnostics",
            json!({
                "snapshot": snapshot,
                "project": project,
                "file": main_ts_path
            }),
        )
        .await?;

    let _ = client.call("release", json!({ "handle": snapshot })).await;

    if diags.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        Ok(())
    } else {
        let formatted = format_semantic_diagnostics(&diags, &excerpt_sources);
        Err(format!("Type errors found:\n{formatted}"))
    }
}

fn format_semantic_diagnostics(diags: &Value, vfs: &HashMap<String, String>) -> String {
    let Some(arr) = diags.as_array() else {
        return diags.to_string();
    };
    arr.iter()
        .map(|d| format_one_semantic_diagnostic(d, vfs))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_one_semantic_diagnostic(diag: &Value, vfs: &HashMap<String, String>) -> String {
    let text = diag["text"].as_str().unwrap_or("Type error");
    let ts_code = diag["code"].as_u64().or_else(|| diag["code"].as_i64().map(|c| c as u64));
    let file_name = diag["fileName"].as_str();
    let pos = diag["pos"].as_u64().map(|p| p as usize);
    let end = diag["end"].as_u64().map(|p| p as usize);

    let mut out = String::new();
    if let Some(code) = ts_code {
        out.push_str(&format!("TS{code}"));
    }

    let (source_key, display_path) = file_name
        .map(resolve_diagnostic_file)
        .unwrap_or((format!("{VIRTUAL_ROOT}/main.ts"), None));

    if let Some(path) = display_path {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&path);
    }

    if let Some(source) = vfs.get(&source_key) {
        if let (Some(pos), Some(line_starts)) = (pos, Some(build_line_starts(source))) {
            let end_pos = end.unwrap_or(pos);
            let (line, col) = line_column_from_starts(&line_starts, pos);
            let (end_line, end_col) = line_column_from_starts(&line_starts, end_pos);
            if !out.is_empty() {
                out.push(' ');
            }
            if line != end_line {
                out.push_str(&format!(
                    "(line {line}, col {col} to line {end_line}, col {end_col})"
                ));
            } else {
                out.push_str(&format!("(line {line}, col {col})"));
            }
            if let Some(excerpt) = excerpt_at_line(source, &line_starts, line) {
                out.push_str(&format!(" — `{excerpt}`"));
            }
        }
    }

    if !out.is_empty() {
        out.push_str(": ");
    }
    out.push_str(text);
    out
}

fn resolve_diagnostic_file(file_name: &str) -> (String, Option<String>) {
    let normalized = Path::new(file_name)
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned();

    let main_ts = format!("{VIRTUAL_ROOT}/main.ts");
    if normalized == main_ts {
        return (main_ts, None);
    }

    let units_prefix = format!("{VIRTUAL_ROOT}/node_modules/@units/");
    let display = if let Some(rest) = normalized.strip_prefix(&units_prefix) {
        format!("@units/{rest}")
    } else if let Some(rest) = normalized.strip_prefix(VIRTUAL_ROOT) {
        rest.trim_start_matches('/').to_string()
    } else {
        Path::new(&normalized)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| normalized.clone())
    };
    (normalized, Some(display))
}

fn build_line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            i += 1;
            starts.push(i);
        } else if b == b'\r' {
            i += 1;
            if i < bytes.len() && bytes[i] == b'\n' {
                i += 1;
            }
            starts.push(i);
        } else if b == 0xE2 && i + 2 < bytes.len() && bytes[i + 1] == 0x80 {
            let third = bytes[i + 2];
            if third == 0xA8 || third == 0xA9 {
                i += 3;
                starts.push(i);
                continue;
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    starts
}

fn line_column_from_starts(line_starts: &[usize], pos: usize) -> (usize, usize) {
    if line_starts.is_empty() {
        return (1, 1);
    }
    let line_idx = match line_starts.binary_search(&pos) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let line_start = line_starts[line_idx];
    (line_idx + 1, pos - line_start + 1)
}

fn excerpt_at_line(source: &str, line_starts: &[usize], line: usize) -> Option<String> {
    let line_idx = line.saturating_sub(1);
    let start = *line_starts.get(line_idx)?;
    let end = line_starts
        .get(line_idx + 1)
        .copied()
        .unwrap_or(source.len());
    let line_text = source.get(start..end)?.trim_end_matches('\r').trim();
    if line_text.is_empty() {
        return None;
    }
    const MAX: usize = 120;
    Some(if line_text.len() > MAX {
        format!("{}…", &line_text[..MAX])
    } else {
        line_text.to_string()
    })
}

// --- IPC Client with Callback Support ---
struct TsgoIpcClient {
    #[allow(dead_code)]
    child: tokio::process::Child,
    writer: BufWriter<tokio::process::ChildStdin>,
    reader: BufReader<tokio::process::ChildStdout>,
    vfs: HashMap<String, String>,
    vfs_dirs: HashSet<String>,
    next_id: u64,
}

impl TsgoIpcClient {
    async fn spawn(vfs: HashMap<String, String>) -> Result<Self, String> {
        let tsgo_bin = find_tsgo_bin()?;
        let mut child = tokio::process::Command::new(&tsgo_bin)
            .args([
                "--api",
                "--async",
                "--callbacks=readFile,fileExists,directoryExists",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;

        let writer = BufWriter::new(child.stdin.take().unwrap());
        let reader = BufReader::new(child.stdout.take().unwrap());

        let vfs_dirs = vfs_dirs_for_keys(&vfs);

        Ok(Self {
            child,
            writer,
            reader,
            vfs,
            vfs_dirs,
            next_id: 1,
        })
    }

    // ---------- Public API ----------
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.to_string();
        self.next_id += 1;

        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_message(&msg).await?;

        loop {
            let resp = self.read_message().await?;

            if resp.get("method").is_some() {
                let cb_id = resp["id"].clone();
                let cb_method = resp["method"].as_str().unwrap();
                let result = self.handle_fs_callback(cb_method, &resp["params"]);
                self.write_message(&json!({
                    "jsonrpc": "2.0",
                    "id": cb_id,
                    "result": result,
                }))
                .await?;
                continue;
            }

            if resp["id"].as_str() == Some(&id) {
                if let Some(err) = resp.get("error") {
                    return Err(format!(
                        "tsgo error: {} (code: {})",
                        err["message"].as_str().unwrap_or("unknown"),
                        err["code"]
                    ));
                }
                return Ok(resp["result"].clone());
            }
        }
    }

    fn handle_fs_callback(&self, method: &str, params: &Value) -> Value {
        vfs_fs_callback(&self.vfs, &self.vfs_dirs, method, params)
    }

    // ---------- JSON-RPC IO ----------
    async fn write_message(&mut self, msg: &Value) -> Result<(), String> {
        let body = msg.to_string();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer
            .write_all(header.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.writer
            .write_all(body.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.writer.flush().await.map_err(|e| e.to_string())
    }

    async fn read_message(&mut self) -> Result<Value, String> {
        let mut len = 0;
        loop {
            let mut line = String::new();
            let bytes_read = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            if bytes_read == 0 {
                return Err("Connection closed by tsgo".to_string());
            }
            if line.trim().is_empty() {
                break;
            }
            if line.starts_with("Content-Length: ") {
                len = line["Content-Length: ".len()..]
                    .trim()
                    .parse()
                    .map_err(|_| "Invalid Content-Length")?;
            }
        }
        let mut body = vec![0u8; len];
        self.reader
            .read_exact(&mut body)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    }
}

fn vfs_fs_callback(
    vfs: &HashMap<String, String>,
    vfs_dirs: &HashSet<String>,
    method: &str,
    params: &Value,
) -> Value {
    let requested_path = match params {
        Value::String(s) => Some(s.as_str()),
        Value::Array(arr) => arr.first().and_then(Value::as_str),
        _ => None,
    }
    .unwrap_or("");

    let full_path = if requested_path.starts_with('/') {
        requested_path.to_string()
    } else if requested_path.is_empty() {
        VIRTUAL_ROOT.to_string()
    } else {
        format!("{}/{}", VIRTUAL_ROOT, requested_path)
    };

    let normalized = Path::new(&full_path).components().collect::<PathBuf>();
    let full_path = normalized
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string();

    match method {
        "readFile" => {
            if vfs_dirs.contains(&full_path) {
                return Value::Null;
            }
            if let Some(content) = vfs.get(&full_path) {
                return json!({ "content": content });
            }
            Value::Null
        }
        "fileExists" => json!(vfs.contains_key(&full_path)),
        "directoryExists" => json!(vfs_dirs.contains(&full_path)),
        _ => {
            eprintln!("⚠️ Unhandled callback: {}", method);
            Value::Null
        }
    }
}

fn vfs_dirs_for_keys(vfs: &HashMap<String, String>) -> HashSet<String> {
    let mut vfs_dirs = HashSet::new();
    vfs_dirs.insert(VIRTUAL_ROOT.to_string());
    for key in vfs.keys() {
        let mut current_path = Path::new(key).parent();
        while let Some(parent) = current_path {
            let parent_str = parent.to_string_lossy().to_string();
            if parent_str.is_empty() || parent_str == "/" {
                break;
            }
            vfs_dirs.insert(parent_str);
            current_path = parent.parent();
        }
    }
    vfs_dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn main_ts_sources() -> HashMap<String, String> {
        HashMap::from([(
            format!("{VIRTUAL_ROOT}/main.ts"),
            "line1\nconst formData = await parseMultipartRequest(req);\n".to_string(),
        )])
    }

    fn diag(
        file_name: Option<&str>,
        pos: Option<usize>,
        end: Option<usize>,
        code: Option<i64>,
        text: &str,
    ) -> Value {
        let mut d = json!({ "text": text });
        if let Some(p) = pos {
            d["pos"] = json!(p);
        }
        if let Some(e) = end {
            d["end"] = json!(e);
        }
        if let Some(c) = code {
            d["code"] = json!(c);
        }
        if let Some(f) = file_name {
            d["fileName"] = json!(f);
        }
        d
    }

    #[test]
    fn format_semantic_diagnostics_empty_array_is_empty() {
        assert_eq!(format_semantic_diagnostics(&json!([]), &main_ts_sources()), "");
    }

    #[test]
    fn format_semantic_diagnostics_non_array_falls_back_to_json_string() {
        let diags = json!({ "unexpected": true });
        assert_eq!(
            format_semantic_diagnostics(&diags, &main_ts_sources()),
            diags.to_string()
        );
    }

    #[test]
    fn format_semantic_diagnostics_joins_multiple_errors() {
        let source = "const a: number = 1;\nconst b: string = 2;\n";
        let mut vfs = HashMap::new();
        vfs.insert(format!("{VIRTUAL_ROOT}/main.ts"), source.to_string());
        let diags = json!([
            diag(Some("/unit/main.ts"), Some(14), Some(15), Some(2322), "err a"),
            diag(Some("/unit/main.ts"), Some(33), Some(34), Some(2322), "err b"),
        ]);
        let out = format_semantic_diagnostics(&diags, &vfs);
        assert!(out.contains("err a"));
        assert!(out.contains("err b"));
        assert_eq!(out.matches('\n').count(), 1);
    }

    #[test]
    fn formats_main_ts_diagnostic_with_line_and_source_excerpt() {
        let vfs = main_ts_sources();
        let source = vfs.get(&format!("{VIRTUAL_ROOT}/main.ts")).unwrap();
        let pos = source.find("req").unwrap();
        let diags = json!([diag(
            Some("/unit/main.ts"),
            Some(pos),
            Some(pos + 3),
            Some(2740),
            "Type 'IncomingMessage' is missing the following properties from type 'Request'",
        )]);

        let out = format_semantic_diagnostics(&diags, &vfs);
        assert!(out.starts_with("TS2740 (line 2, col "));
        assert!(out.contains("parseMultipartRequest(req);`"));
        assert!(out.contains("Type 'IncomingMessage'"));
        assert!(!out.contains("/unit/main.ts"));
        assert!(!out.contains("fileName"));
    }

    #[test]
    fn formats_units_import_path_without_virtual_root() {
        let unit_path = format!("{VIRTUAL_ROOT}/node_modules/@units/auth.core.api.signup.ts");
        let source = "export const x: number = \"bad\";\n";
        let pos = source.find("\"bad\"").unwrap();
        let mut vfs = HashMap::new();
        vfs.insert(unit_path.clone(), source.to_string());
        let diags = json!([diag(
            Some(&unit_path),
            Some(pos),
            Some(pos + 5),
            Some(2322),
            "Type 'string' is not assignable to type 'number'.",
        )]);

        let out = format_semantic_diagnostics(&diags, &vfs);
        assert!(out.starts_with("TS2322 @units/auth.core.api.signup.ts"));
        assert!(out.contains("(line 1, col "));
        assert!(out.contains("— `export const x: number = \"bad\";`"));
        assert!(!out.contains(VIRTUAL_ROOT));
    }

    #[test]
    fn format_one_defaults_missing_fields_to_main_ts_and_type_error() {
        let out = format_one_semantic_diagnostic(&json!({}), &HashMap::new());
        assert_eq!(out, "Type error");
    }

    #[test]
    fn format_one_without_pos_omits_line_and_excerpt() {
        let vfs = main_ts_sources();
        let out = format_one_semantic_diagnostic(
            &diag(Some("/unit/main.ts"), None, None, Some(9999), "no position"),
            &vfs,
        );
        assert_eq!(out, "TS9999: no position");
        assert!(!out.contains("line"));
    }

    #[test]
    fn format_one_without_source_in_vfs_omits_line_even_with_pos() {
        let out = format_one_semantic_diagnostic(
            &diag(Some("/unit/main.ts"), Some(0), Some(1), Some(1), "missing vfs"),
            &HashMap::new(),
        );
        assert_eq!(out, "TS1: missing vfs");
    }

    #[test]
    fn format_one_negative_code_as_i64() {
        let vfs = main_ts_sources();
        let out = format_one_semantic_diagnostic(
            &json!({ "code": -1, "text": "neg", "pos": 0, "fileName": "/unit/main.ts" }),
            &vfs,
        );
        assert!(out.starts_with("TS18446744073709551615"));
    }

    #[test]
    fn format_one_without_ts_code_or_file_name() {
        let vfs = main_ts_sources();
        let out = format_one_semantic_diagnostic(
            &json!({ "text": "bare", "pos": 0 }),
            &vfs,
        );
        assert!(out.starts_with("(line 1, col 1)"));
        assert!(out.ends_with(": bare"));
        assert!(!out.contains("TS"));
    }

    #[test]
    fn format_one_multiline_span() {
        let source = "line one\nline two\nline three\n";
        let mut vfs = HashMap::new();
        vfs.insert(format!("{VIRTUAL_ROOT}/main.ts"), source.to_string());
        let start = source.find("line two").unwrap();
        let end = source.find("line three").unwrap() + "line".len();
        let out = format_one_semantic_diagnostic(
            &diag(Some("/unit/main.ts"), Some(start), Some(end), Some(1), "span"),
            &vfs,
        );
        assert!(out.contains("(line 2, col 1 to line 3, col 5)"));
        assert!(out.contains("— `line two`"));
    }

    #[test]
    fn resolve_diagnostic_file_normalizes_main_and_units_and_other_paths() {
        assert_eq!(
            resolve_diagnostic_file("/unit/main.ts"),
            (format!("{VIRTUAL_ROOT}/main.ts"), None)
        );
        let unit = format!("{VIRTUAL_ROOT}/node_modules/@units/a.b.c.d.ts");
        assert_eq!(
            resolve_diagnostic_file(&unit),
            (unit.clone(), Some("@units/a.b.c.d.ts".to_string()))
        );
        assert_eq!(
            resolve_diagnostic_file("/unit/tsconfig.json"),
            (
                "/unit/tsconfig.json".to_string(),
                Some("tsconfig.json".to_string())
            )
        );
        assert_eq!(
            resolve_diagnostic_file("/elsewhere/foo.ts"),
            (
                "/elsewhere/foo.ts".to_string(),
                Some("foo.ts".to_string())
            )
        );
        let parent_dot = resolve_diagnostic_file("/unit/../unit/main.ts");
        assert_eq!(parent_dot.0, "/unit/../unit/main.ts");
        assert_eq!(parent_dot.1.as_deref(), Some("../unit/main.ts"));
    }

    #[test]
    fn line_column_handles_crlf_and_leading_newline() {
        let crlf = "  try {\r\n    const x: number = \"bad\";\r\n";
        let pos = crlf.find("\"bad\"").unwrap();
        let starts = build_line_starts(crlf);
        assert_eq!(line_column_from_starts(&starts, pos), (2, 23));

        let prefixed = "\nconst x: number = 1;\n";
        let pos = prefixed.find("const").unwrap();
        assert_eq!(
            line_column_from_starts(&build_line_starts(prefixed), pos),
            (2, 1)
        );
    }

    #[test]
    fn line_column_handles_unicode_line_separators() {
        let source = format!("a\u{2028}b\nc");
        let pos = source.find('b').unwrap();
        assert_eq!(
            line_column_from_starts(&build_line_starts(&source), pos),
            (2, 1)
        );
    }

    #[test]
    fn line_column_empty_starts_returns_one_one() {
        assert_eq!(line_column_from_starts(&[], 5), (1, 1));
    }

    #[test]
    fn excerpt_omits_empty_lines_and_truncates_long_lines() {
        let source = "  // only comment\n\nconst x = 1;\n";
        let starts = build_line_starts(source);
        assert!(excerpt_at_line(source, &starts, 2).is_none());

        let long = format!("// {}\n", "x".repeat(130));
        let starts = build_line_starts(&long);
        let excerpt = excerpt_at_line(&long, &starts, 1).unwrap();
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.chars().count() <= 121);
    }

    #[test]
    fn run_typescript_check_error_prefix_includes_newline() {
        let vfs = main_ts_sources();
        let source = vfs.get(&format!("{VIRTUAL_ROOT}/main.ts")).unwrap();
        let pos = source.find("req").unwrap();
        let diags = json!([diag(
            Some("/unit/main.ts"),
            Some(pos),
            Some(pos + 3),
            Some(2740),
            "err",
        )]);
        let formatted = format_semantic_diagnostics(&diags, &vfs);
        let err = format!("Type errors found:\n{formatted}");
        assert!(err.starts_with("Type errors found:\nTS2740"));
    }

    #[test]
    fn vfs_fs_callback_read_file_exists_and_directory() {
        let mut vfs = HashMap::new();
        vfs.insert(format!("{VIRTUAL_ROOT}/main.ts"), "export {}\n".to_string());
        let dirs = vfs_dirs_for_keys(&vfs);

        let content = vfs_fs_callback(&vfs, &dirs, "readFile", &json!("/unit/main.ts"));
        assert_eq!(content["content"], "export {}\n");

        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "readFile", &json!(VIRTUAL_ROOT)),
            Value::Null
        );
        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "readFile", &json!("/unit/missing.ts")),
            Value::Null
        );
    }

    #[test]
    fn vfs_fs_callback_file_and_directory_exists() {
        let mut vfs = HashMap::new();
        vfs.insert(format!("{VIRTUAL_ROOT}/main.ts"), "x".to_string());
        let dirs = vfs_dirs_for_keys(&vfs);

        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "fileExists", &json!("/unit/main.ts")),
            json!(true)
        );
        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "fileExists", &json!("/unit/missing.ts")),
            json!(false)
        );
        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "directoryExists", &json!("/unit")),
            json!(true)
        );
        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "directoryExists", &json!("/unit/missing")),
            json!(false)
        );
    }

    #[test]
    fn vfs_fs_callback_resolves_relative_and_array_params() {
        let mut vfs = HashMap::new();
        vfs.insert(format!("{VIRTUAL_ROOT}/main.ts"), "rel".to_string());
        let dirs = vfs_dirs_for_keys(&vfs);

        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "readFile", &json!(["main.ts"])),
            json!({ "content": "rel" })
        );
        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "readFile", &json!("")),
            Value::Null
        );
    }

    #[test]
    fn vfs_fs_callback_unknown_method_returns_null() {
        let vfs = HashMap::new();
        let dirs = vfs_dirs_for_keys(&vfs);
        assert_eq!(
            vfs_fs_callback(&vfs, &dirs, "unknownMethod", &json!("/unit/main.ts")),
            Value::Null
        );
    }

    #[test]
    fn vfs_dirs_for_keys_includes_parents() {
        let mut vfs = HashMap::new();
        vfs.insert(
            format!("{VIRTUAL_ROOT}/node_modules/@units/a.b.c.d.ts"),
            "x".to_string(),
        );
        let dirs = vfs_dirs_for_keys(&vfs);
        assert!(dirs.contains(VIRTUAL_ROOT));
        assert!(dirs.contains(&format!("{VIRTUAL_ROOT}/node_modules")));
        assert!(dirs.contains(&format!("{VIRTUAL_ROOT}/node_modules/@units")));
    }
}

