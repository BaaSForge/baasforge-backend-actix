use uuid::Uuid;

#[cfg(not(feature = "unit_technical_reviewer"))]
pub async fn run_type_lint_check(
    _pool: &crate::database::DbPool,
    _project_id: Uuid,
    _session_id: Uuid,
    _code: &str,
) -> Result<(), String> {
    Ok(())
}

#[cfg(feature = "unit_technical_reviewer")]
mod imp {
    use super::Uuid;
    use crate::database::{DbPool, block_on_db_tokio};
    use crate::services::chat_worker::db::load_effective_units_for_chat_session;
    use crate::services::unit_technical_reviewer::type_lints;
    use crate::services::unit_technical_reviewer::unit_vfs::{
        self, find_project_root, find_tsgolint_bin, materialize_for_disk_lint,
    };
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::fs;
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION: &str = "no-unsafe-type-assertion";

    /// SDK branded runtime IDs (`@sdk.d.ts`); `as T` from `string` is allowed for these only.
    const RELAXED_SDK_BRANDED_ID_TYPES: &[&str] = &[
        "ActorInstanceId",
        "WorkflowInstanceId",
        "QueueWorkerRunId",
    ];

    pub async fn run_type_lint_check(
        pool: &DbPool,
        project_id: Uuid,
        session_id: Uuid,
        code: &str,
    ) -> Result<(), String> {
        let code = normalize_ts_source_for_lint(code);
        type_lints::run_syntax_lint_checks(&code)?;

        let units = block_on_db_tokio(pool, move |conn| {
            load_effective_units_for_chat_session(conn, project_id, session_id)
        })
        .await
        .map_err(|e| format!("Failed to load units for lint check: {}", e))?;

        let vfs = unit_vfs::build_vfs(&code, &units);
        let project_root = find_project_root()?;

        tokio::task::spawn_blocking(move || run_tsgolint_headless(&project_root, vfs))
            .await
            .map_err(|e| format!("Lint task join error: {e}"))?
    }

    fn normalize_ts_source_for_lint(code: &str) -> String {
        let code = code.strip_prefix('\u{FEFF}').unwrap_or(code);
        code.replace("\r\n", "\n").replace('\r', "\n")
    }

    fn vfs_sources_by_rel_path(vfs: &HashMap<String, String>) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (vfs_path, content) in vfs {
            let Some(rel) = vfs_path.strip_prefix("/unit/") else {
                continue;
            };
            if rel == "tsconfig.json" || rel == "package.json" {
                continue;
            }
            if rel == "node_modules" || rel.starts_with("node_modules/") {
                if !rel.contains("/@units/") && !rel.ends_with("/@units") {
                    continue;
                }
            }
            out.insert(rel.to_string(), content.clone());
        }
        out
    }

    fn load_sources_from_materialized(
        root: &Path,
        fallback: &HashMap<String, String>,
    ) -> Result<HashMap<String, String>, String> {
        let mut out = HashMap::new();
        for rel in fallback.keys() {
            let path = root.join(rel);
            if path.is_file() {
                let content = fs::read_to_string(&path).map_err(|e| e.to_string())?;
                out.insert(rel.clone(), content);
            }
        }
        if out.is_empty() {
            Ok(fallback.clone())
        } else {
            Ok(out)
        }
    }

    fn run_tsgolint_headless(
        project_root: &Path,
        vfs: HashMap<String, String>,
    ) -> Result<(), String> {
        let vfs_sources = vfs_sources_by_rel_path(&vfs);
        let materialized = materialize_for_disk_lint(project_root, &vfs)?;
        let materialized_root = materialized
            .root
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let sources_by_rel = load_sources_from_materialized(&materialized_root, &vfs_sources)?;
        let lint_ctx = LintFormatContext::new(materialized_root, sources_by_rel);
        let main_path = materialized
            .main_ts
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let main_path_str = main_path.to_string_lossy().into_owned();
        let main_source = lint_ctx
            .sources_by_rel
            .get("main.ts")
            .cloned()
            .ok_or("materialized project is missing main.ts source")?;

        let payload = TsgolintPayload {
            version: 2,
            configs: vec![TsgolintConfig {
                file_paths: vec![main_path_str.clone()],
                rules: vec![TsgolintRule {
                    name: TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION.to_string(),
                    options: None,
                }],
            }],
            source_overrides: Some(HashMap::from([(main_path_str, main_source)])),
            report_syntactic: false,
            report_semantic: false,
        };

        let tsgolint_bin = find_tsgolint_bin()?;
        let json_input = serde_json::to_string(&payload).map_err(|e| e.to_string())?;

        let mut child = Command::new(&tsgolint_bin)
            .arg("headless")
            .current_dir(&materialized.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn tsgolint at {}: {e}", tsgolint_bin.display()))?;

        child
            .stdin
            .take()
            .ok_or("tsgolint stdin unavailable")?
            .write_all(json_input.as_bytes())
            .map_err(|e| format!("Failed to write tsgolint stdin: {e}"))?;

        let mut stdout = child.stdout.take().ok_or("tsgolint stdout unavailable")?;
        let mut stderr = child.stderr.take().ok_or("tsgolint stderr unavailable")?;
        let mut messages = Vec::new();
        parse_tsgolint_stdout(&mut stdout, &lint_ctx, &mut messages)?;

        let mut stderr_buf = String::new();
        let _ = stderr.read_to_string(&mut stderr_buf);

        let status = child.wait().map_err(|e| e.to_string())?;

        if !status.success() {
            return Err(format!(
                "tsgolint exited with {status}. stderr: {}",
                stderr_buf.trim()
            ));
        }

        if messages.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Type-aware lint violations:\n{}",
                messages.join("\n")
            ))
        }
    }

    #[derive(Debug, Serialize)]
    struct TsgolintPayload {
        version: i32,
        configs: Vec<TsgolintConfig>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_overrides: Option<std::collections::HashMap<String, String>>,
        report_syntactic: bool,
        report_semantic: bool,
    }

    #[derive(Debug, Serialize)]
    struct TsgolintConfig {
        file_paths: Vec<String>,
        rules: Vec<TsgolintRule>,
    }

    #[derive(Debug, Serialize)]
    struct TsgolintRule {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        options: Option<serde_json::Value>,
    }

    #[derive(Debug, Deserialize)]
    struct TsgolintRuleMessage {
        #[serde(default)]
        id: Option<String>,
        description: String,
        #[serde(default)]
        help: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct TsgolintRange {
        pos: u32,
        end: u32,
    }

    struct LintFormatContext {
        materialized_root: PathBuf,
        sources_by_rel: HashMap<String, String>,
        line_starts_by_rel: HashMap<String, Vec<usize>>,
    }

    impl LintFormatContext {
        fn new(materialized_root: PathBuf, sources_by_rel: HashMap<String, String>) -> Self {
            let line_starts_by_rel = sources_by_rel
                .iter()
                .map(|(rel, source)| (rel.clone(), build_line_starts(source)))
                .collect();
            Self {
                materialized_root,
                sources_by_rel,
                line_starts_by_rel,
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TsgolintDiagnosticKind {
        Rule = 0,
        Tsconfig = 1,
    }

    impl<'de> Deserialize<'de> for TsgolintDiagnosticKind {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            match u8::deserialize(deserializer)? {
                0 => Ok(Self::Rule),
                1 => Ok(Self::Tsconfig),
                other => Err(serde::de::Error::custom(format!(
                    "unknown tsgolint diagnostic kind: {other}"
                ))),
            }
        }
    }

    #[derive(Debug, Deserialize)]
    struct TsgolintDiagnosticPayload {
        kind: TsgolintDiagnosticKind,
        #[serde(default)]
        rule: Option<String>,
        message: TsgolintRuleMessage,
        #[serde(default)]
        file_path: Option<String>,
        #[serde(default)]
        range: Option<TsgolintRange>,
    }

    #[derive(Debug, Deserialize)]
    struct TsgolintErrorPayload {
        error: String,
    }

    enum TsgolintStreamMessage {
        Error(String),
        Diagnostic(TsgolintDiagnosticPayload),
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

    fn display_path(file_path: &str, ctx: &LintFormatContext) -> String {
        let path = Path::new(file_path);
        if let Ok(rel) = path.strip_prefix(&ctx.materialized_root) {
            return rel.display().to_string();
        }
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "main.ts".to_string())
    }

    fn source_for_display_path<'a>(
        display_path: &str,
        ctx: &'a LintFormatContext,
    ) -> Option<&'a str> {
        ctx.sources_by_rel.get(display_path).map(String::as_str)
    }

    fn slice_asserts_to_branded_id(slice: &str, type_name: &str) -> bool {
        let needle = format!("as {type_name}");
        let Some(idx) = slice.find(&needle) else {
            return false;
        };
        let after = idx + needle.len();
        match slice.as_bytes().get(after) {
            None => true,
            Some(b) => !b.is_ascii_alphanumeric() && *b != b'_',
        }
    }

    fn is_relaxed_sdk_branded_id_type_assertion(
        ctx: &LintFormatContext,
        d: &TsgolintDiagnosticPayload,
    ) -> bool {
        for type_name in RELAXED_SDK_BRANDED_ID_TYPES {
            if d.message.description.contains(&format!("type '{type_name}'")) {
                return true;
            }
        }

        let display_path = d
            .file_path
            .as_deref()
            .map(|p| display_path(p, ctx))
            .unwrap_or_else(|| "main.ts".to_string());
        let Some(source) = source_for_display_path(&display_path, ctx) else {
            return false;
        };
        let Some(range) = d.range.as_ref() else {
            return false;
        };
        let pos = range.pos as usize;
        let end = range.end as usize;
        let Some(slice) = source.get(pos..end) else {
            return false;
        };
        RELAXED_SDK_BRANDED_ID_TYPES
            .iter()
            .any(|type_name| slice_asserts_to_branded_id(slice, type_name))
    }

    fn excerpt_at_line(
        source: &str,
        line_starts: &[usize],
        line: usize,
    ) -> Option<String> {
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

    fn format_tsgolint_violation(d: &TsgolintDiagnosticPayload, ctx: &LintFormatContext) -> String {
        let default_path = if matches!(d.kind, TsgolintDiagnosticKind::Tsconfig) {
            "tsconfig.json"
        } else {
            "main.ts"
        };
        let display_path = d
            .file_path
            .as_deref()
            .map(|p| display_path(p, ctx))
            .unwrap_or_else(|| default_path.to_string());

        let source = source_for_display_path(&display_path, ctx);
        let mut out = String::new();
        if let Some(rule) = d.rule.as_deref().filter(|r| !r.is_empty()) {
            out.push_str(rule);
            out.push_str(" in ");
        }
        out.push_str(&display_path);

        if let (Some(source), Some(range), Some(line_starts)) = (
            source,
            d.range.as_ref(),
            ctx.line_starts_by_rel.get(&display_path),
        ) {
            let pos = range.pos as usize;
            let end = range.end as usize;
            let (line, col) = line_column_from_starts(line_starts, pos);
            let (end_line, end_col) = line_column_from_starts(line_starts, end);
            if line != end_line {
                out.push_str(&format!(
                    " (line {line}, col {col} to line {end_line}, col {end_col})"
                ));
            } else {
                out.push_str(&format!(" (line {line}, col {col})"));
            }
            if let Some(excerpt) = excerpt_at_line(source, line_starts, line) {
                out.push_str(&format!(" — `{excerpt}`"));
            }
        }

        out.push_str(": ");
        out.push_str(&d.message.description);
        if let Some(help) = d.message.help.as_deref().filter(|h| !h.is_empty()) {
            out.push_str(&format!(" ({help})"));
        }
        out
    }

    fn parse_tsgolint_stdout(
        stdout: &mut impl Read,
        ctx: &LintFormatContext,
        violations: &mut Vec<String>,
    ) -> Result<(), String> {
        let mut buffer = Vec::new();
        let mut read_buf = [0u8; 8192];

        loop {
            match stdout.read(&mut read_buf) {
                Ok(0) => break,
                Ok(n) => buffer.extend_from_slice(&read_buf[..n]),
                Err(e) => return Err(format!("Failed to read tsgolint stdout: {e}")),
            }

            while let Some(msg) = try_parse_next_message(&mut buffer)? {
                match msg {
                    TsgolintStreamMessage::Error(err) => return Err(err),
                    TsgolintStreamMessage::Diagnostic(d) => {
                        let is_target_rule = matches!(d.kind, TsgolintDiagnosticKind::Rule)
                            && d.rule.as_deref() == Some(TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION);
                        let is_tsconfig = matches!(d.kind, TsgolintDiagnosticKind::Tsconfig);
                        if is_target_rule {
                            if is_relaxed_sdk_branded_id_type_assertion(ctx, &d) {
                                continue;
                            }
                            violations.push(format_tsgolint_violation(&d, ctx));
                        } else if is_tsconfig {
                            violations.push(format_tsgolint_violation(&d, ctx));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn try_parse_next_message(
        buffer: &mut Vec<u8>,
    ) -> Result<Option<TsgolintStreamMessage>, String> {
        if buffer.len() < 5 {
            return Ok(None);
        }

        let size = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        let message_type = buffer[4];
        let frame_len = 5 + size;

        if buffer.len() < frame_len {
            return Ok(None);
        }

        let payload_bytes = buffer[5..frame_len].to_vec();
        buffer.drain(..frame_len);

        let payload_str = String::from_utf8(payload_bytes)
            .map_err(|e| format!("Invalid tsgolint payload utf-8: {e}"))?;

        let msg = match message_type {
            0 => {
                let err: TsgolintErrorPayload = serde_json::from_str(&payload_str)
                    .map_err(|e| format!("tsgolint error JSON: {e}; payload: {payload_str}"))?;
                TsgolintStreamMessage::Error(err.error)
            }
            1 => {
                let diag: TsgolintDiagnosticPayload = serde_json::from_str(&payload_str)
                    .map_err(|e| format!("tsgolint diagnostic JSON: {e}; payload: {payload_str}"))?;
                TsgolintStreamMessage::Diagnostic(diag)
            }
            2 => return try_parse_next_message(buffer),
            other => return Err(format!("Unknown tsgolint message type: {other}")),
        };

        Ok(Some(msg))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::HashMap;
        use std::io::Cursor;
        use std::path::PathBuf;

        fn ctx_for_source(source: &str) -> LintFormatContext {
            LintFormatContext::new(
                PathBuf::from("/tmp/lint-project"),
                HashMap::from([("main.ts".to_string(), source.to_string())]),
            )
        }

        fn rule_diagnostic(
            source: &str,
            pos: u32,
            end: u32,
            file_path: Option<&str>,
        ) -> TsgolintDiagnosticPayload {
            TsgolintDiagnosticPayload {
                kind: TsgolintDiagnosticKind::Rule,
                rule: Some(TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION.to_string()),
                message: TsgolintRuleMessage {
                    id: None,
                    description: "unsafe".to_string(),
                    help: None,
                },
                file_path: file_path
                    .map(str::to_string)
                    .or_else(|| Some("/tmp/lint-project/main.ts".to_string())),
                range: Some(TsgolintRange { pos, end }),
            }
        }

        fn encode_tsgolint_frame(message_type: u8, payload_json: &str) -> Vec<u8> {
            let payload = payload_json.as_bytes();
            let mut frame = Vec::with_capacity(5 + payload.len());
            frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            frame.push(message_type);
            frame.extend_from_slice(payload);
            frame
        }

        #[test]
        fn normalize_strips_bom_and_unifies_line_endings() {
            assert_eq!(normalize_ts_source_for_lint("\u{FEFF}const x = 1;\n"), "const x = 1;\n");
            assert_eq!(
                normalize_ts_source_for_lint("a\r\nb\rc\n"),
                "a\nb\nc\n"
            );
        }

        #[test]
        fn line_column_handles_crlf_and_leading_newline() {
            let crlf = "  try {\r\n    const x = y as string;\r\n";
            let pos = crlf.find("y as string").unwrap();
            let starts = build_line_starts(crlf);
            let (line, col) = line_column_from_starts(&starts, pos);
            assert_eq!((line, col), (2, 15));

            let prefixed = "\nconst x = y as string;\n";
            let pos = prefixed.find("y as string").unwrap();
            let starts = build_line_starts(prefixed);
            assert_eq!(line_column_from_starts(&starts, pos).0, 2);
        }

        #[test]
        fn line_column_handles_unicode_line_separators() {
            let source = format!("a\u{2028}b\nc");
            let pos = source.find('b').unwrap();
            let starts = build_line_starts(&source);
            assert_eq!(line_column_from_starts(&starts, pos), (2, 1));
        }

        #[test]
        fn line_number_counts_comment_lines() {
            let source = "  try {\n    // comment line\n    const x = y as string;\n";
            let pos = source.find("y as string").unwrap();
            let line_starts = build_line_starts(source);
            let (line, _) = line_column_from_starts(&line_starts, pos);
            assert_eq!(line, 3, "assertion is on the third line when a comment precedes it");
        }

        #[test]
        fn line_number_counts_blank_line_before_comment() {
            let source = "  try {\n\n    // comment line\n    const x = y as string;\n";
            let pos = source.find("y as string").unwrap();
            let line_starts = build_line_starts(source);
            let (line, _) = line_column_from_starts(&line_starts, pos);
            assert_eq!(
                line, 4,
                "blank line and comment must both increment the reported line"
            );
        }

        #[test]
        fn line_number_counts_block_comment_lines() {
            let source = "/* block\n   comment */\nconst x = y as string;\n";
            let pos = source.find("y as string").unwrap();
            let (line, _) = line_column_from_starts(&build_line_starts(source), pos);
            assert_eq!(line, 3);
        }

        #[test]
        fn signup_fixture_matches_tsgolint_byte_offset() {
            let source = include_str!("lint_signup_api.ts");
            let normalized = normalize_ts_source_for_lint(source);
            let pos = normalized.find("email as ActorInstanceId").unwrap();
            assert_eq!(pos, 1045);
            let (line, col) = line_column_from_starts(&build_line_starts(&normalized), pos);
            assert_eq!((line, col), (36, 21));
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
        fn formats_violation_with_relative_path_line_and_excerpt() {
            let ctx = ctx_for_source("const x = JSON.parse(\"{}\") as string;\n");
            let d = TsgolintDiagnosticPayload {
                kind: TsgolintDiagnosticKind::Rule,
                rule: Some("no-unsafe-type-assertion".to_string()),
                message: TsgolintRuleMessage {
                    id: None,
                    description:
                        "Unsafe assertion from `any` detected: consider using type guards."
                            .to_string(),
                    help: None,
                },
                file_path: Some("/tmp/lint-project/main.ts".to_string()),
                range: Some(TsgolintRange { pos: 10, end: 36 }),
            };
            let formatted = format_tsgolint_violation(&d, &ctx);
            assert!(
                formatted.contains("no-unsafe-type-assertion in main.ts"),
                "got: {formatted}"
            );
            assert!(formatted.contains("(line 1, col 11"), "got: {formatted}");
            assert!(formatted.contains("JSON.parse"));
            assert!(!formatted.contains("/tmp/lint-project"));
        }

        #[test]
        fn formats_multiline_range_and_help() {
            let source = "line one\nline two\nline three\n";
            let ctx = ctx_for_source(source);
            let start = source.find("line two").unwrap() as u32;
            let end = source.find("line three").unwrap() as u32 + "line".len() as u32;
            let mut d = rule_diagnostic(source, start, end, None);
            d.message.help = Some("use a type guard".to_string());
            let formatted = format_tsgolint_violation(&d, &ctx);
            assert!(formatted.contains("(line 2, col 1 to line 3, col 5)"));
            assert!(formatted.contains("(use a type guard)"));
            assert!(formatted.contains("— `line two`"));
        }

        #[test]
        fn display_path_strips_materialized_root_or_falls_back_to_basename() {
            let ctx = ctx_for_source("const x = 1;\n");
            assert_eq!(
                display_path("/tmp/lint-project/main.ts", &ctx),
                "main.ts"
            );
            assert_eq!(
                display_path("/other/path/main.ts", &ctx),
                "main.ts"
            );
        }

        #[test]
        fn load_sources_from_materialized_prefers_disk_bytes() {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("main.ts"), "const x = 1;\n").unwrap();
            let fallback =
                HashMap::from([("main.ts".to_string(), "const x = 2;\n".to_string())]);
            let loaded = load_sources_from_materialized(dir.path(), &fallback).unwrap();
            assert_eq!(loaded["main.ts"], "const x = 1;\n");
        }

        #[test]
        fn parse_tsgolint_stdout_filters_rules_and_formats() {
            let source = "const x = JSON.parse(\"{}\") as string;\n";
            let ctx = ctx_for_source(source);
            let diag_json = serde_json::json!({
                "kind": 0,
                "rule": TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION,
                "message": { "description": "unsafe assertion" },
                "file_path": "/tmp/lint-project/main.ts",
                "range": { "pos": 10, "end": 36 }
            });
            let other_rule_json = serde_json::json!({
                "kind": 0,
                "rule": "some-other-rule",
                "message": { "description": "ignored" },
                "file_path": "/tmp/lint-project/main.ts",
                "range": { "pos": 0, "end": 1 }
            });
            let mut data = encode_tsgolint_frame(1, &other_rule_json.to_string());
            data.extend(encode_tsgolint_frame(1, &diag_json.to_string()));
            let mut violations = Vec::new();
            parse_tsgolint_stdout(&mut Cursor::new(data), &ctx, &mut violations).unwrap();
            assert_eq!(violations.len(), 1);
            assert!(violations[0].contains("no-unsafe-type-assertion in main.ts"));
            assert!(violations[0].contains("JSON.parse"));
        }

        #[test]
        fn parse_tsgolint_stdout_skips_relaxed_sdk_branded_id_assertions() {
            let source = "const actorId = email as ActorInstanceId;\n";
            let ctx = ctx_for_source(source);
            let pos = source.find("email as ActorInstanceId").unwrap() as u32;
            let end = pos + "email as ActorInstanceId".len() as u32;
            let diag_json = serde_json::json!({
                "kind": 0,
                "rule": TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION,
                "message": {
                    "description": "Unsafe type assertion: type 'ActorInstanceId' is more narrow than the original type."
                },
                "file_path": "/tmp/lint-project/main.ts",
                "range": { "pos": pos, "end": end }
            });
            let data = encode_tsgolint_frame(1, &diag_json.to_string());
            let mut violations = Vec::new();
            parse_tsgolint_stdout(&mut Cursor::new(data), &ctx, &mut violations).unwrap();
            assert!(violations.is_empty());
        }

        #[test]
        fn parse_tsgolint_stdout_still_reports_other_unsafe_assertions() {
            let source = "const x = (body as RequestBody);\n";
            let ctx = ctx_for_source(source);
            let pos = source.find("body as RequestBody").unwrap() as u32;
            let end = pos + "body as RequestBody".len() as u32;
            let diag_json = serde_json::json!({
                "kind": 0,
                "rule": TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION,
                "message": { "description": "unsafe assertion" },
                "file_path": "/tmp/lint-project/main.ts",
                "range": { "pos": pos, "end": end }
            });
            let data = encode_tsgolint_frame(1, &diag_json.to_string());
            let mut violations = Vec::new();
            parse_tsgolint_stdout(&mut Cursor::new(data), &ctx, &mut violations).unwrap();
            assert_eq!(violations.len(), 1);
        }

        #[test]
        fn parse_tsgolint_stdout_error_frame_fails() {
            let ctx = ctx_for_source("const x = 1;\n");
            let err_json = r#"{"error":"tsconfig missing"}"#;
            let data = encode_tsgolint_frame(0, err_json);
            let mut violations = Vec::new();
            let err = parse_tsgolint_stdout(&mut Cursor::new(data), &ctx, &mut violations)
                .unwrap_err();
            assert!(err.contains("tsconfig missing"));
            assert!(violations.is_empty());
        }
    }

}

#[cfg(feature = "unit_technical_reviewer")]
pub use imp::run_type_lint_check;
