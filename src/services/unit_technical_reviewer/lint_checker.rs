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
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION: &str = "no-unsafe-type-assertion";

    pub async fn run_type_lint_check(
        pool: &DbPool,
        project_id: Uuid,
        session_id: Uuid,
        code: &str,
    ) -> Result<(), String> {
        type_lints::run_syntax_lint_checks(code)?;

        let units = block_on_db_tokio(pool, move |conn| {
            load_effective_units_for_chat_session(conn, project_id, session_id)
        })
        .await
        .map_err(|e| format!("Failed to load units for lint check: {}", e))?;

        let vfs = unit_vfs::build_vfs(code, &units);
        let project_root = find_project_root()?;

        tokio::task::spawn_blocking(move || run_tsgolint_headless(&project_root, vfs))
            .await
            .map_err(|e| format!("Lint task join error: {e}"))?
    }

    fn vfs_sources_by_rel_path(
        vfs: &std::collections::HashMap<String, String>,
    ) -> std::collections::HashMap<String, String> {
        let mut out = std::collections::HashMap::new();
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

    fn run_tsgolint_headless(
        project_root: &Path,
        vfs: std::collections::HashMap<String, String>,
    ) -> Result<(), String> {
        let sources_by_rel = vfs_sources_by_rel_path(&vfs);
        let materialized = materialize_for_disk_lint(project_root, &vfs)?;
        let materialized_root = materialized
            .root
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let main_path = materialized
            .main_ts
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let main_path_str = main_path.to_string_lossy().into_owned();
        let lint_ctx = LintFormatContext {
            materialized_root,
            sources_by_rel,
        };

        let payload = TsgolintPayload {
            version: 2,
            configs: vec![TsgolintConfig {
                file_paths: vec![main_path_str],
                rules: vec![TsgolintRule {
                    name: TSGOLINT_RULE_NO_UNSAFE_TYPE_ASSERTION.to_string(),
                    options: None,
                }],
            }],
            source_overrides: None,
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
        sources_by_rel: std::collections::HashMap<String, String>,
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

    fn byte_offset_to_line_column(source: &str, byte_offset: usize) -> (usize, usize) {
        let byte_offset = byte_offset.min(source.len());
        let prefix = &source[..byte_offset];
        let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let column = byte_offset.saturating_sub(line_start) + 1;
        (line, column)
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

    fn excerpt_at_line(source: &str, line: usize) -> Option<String> {
        let line_text = source.lines().nth(line.saturating_sub(1))?.trim();
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

        if let (Some(source), Some(range)) = (source, d.range.as_ref()) {
            let (line, col) = byte_offset_to_line_column(source, range.pos as usize);
            let (end_line, end_col) = byte_offset_to_line_column(source, range.end as usize);
            if line != end_line {
                out.push_str(&format!(
                    " (line {line}, col {col} to line {end_line}, col {end_col})"
                ));
            } else {
                out.push_str(&format!(" (line {line}, col {col})"));
            }
            if let Some(excerpt) = excerpt_at_line(source, line) {
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
                        if is_target_rule || is_tsconfig {
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
        use std::path::PathBuf;

        #[test]
        fn formats_violation_with_relative_path_line_and_excerpt() {
            let ctx = LintFormatContext {
                materialized_root: PathBuf::from("/tmp/lint-project"),
                sources_by_rel: HashMap::from([(
                    "main.ts".to_string(),
                    "const x = JSON.parse(\"{}\") as string;\n".to_string(),
                )]),
            };
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
            assert!(
                formatted.contains("(line 1, col 11"),
                "got: {formatted}"
            );
            assert!(formatted.contains("JSON.parse"));
            assert!(!formatted.contains("/tmp/lint-project"));
        }
    }

}

#[cfg(feature = "unit_technical_reviewer")]
pub use imp::run_type_lint_check;
