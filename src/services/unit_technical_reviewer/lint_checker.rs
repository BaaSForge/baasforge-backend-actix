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

    fn run_tsgolint_headless(
        project_root: &Path,
        vfs: std::collections::HashMap<String, String>,
    ) -> Result<(), String> {
        let materialized = materialize_for_disk_lint(project_root, &vfs)?;
        let main_path = materialized
            .main_ts
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let main_path_str = main_path.to_string_lossy().into_owned();

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
        parse_tsgolint_stdout(&mut stdout, &mut messages)?;

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
        description: String,
        #[serde(default)]
        help: Option<String>,
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
    }

    #[derive(Debug, Deserialize)]
    struct TsgolintErrorPayload {
        error: String,
    }

    enum TsgolintStreamMessage {
        Error(String),
        Diagnostic(TsgolintDiagnosticPayload),
    }

    fn parse_tsgolint_stdout(
        stdout: &mut impl Read,
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
                            let default_path = if is_tsconfig {
                                "tsconfig.json"
                            } else {
                                "main.ts"
                            };
                            let path = d.file_path.unwrap_or_else(|| default_path.to_string());
                            let mut line = format!("{}: {}", path, d.message.description);
                            if let Some(help) = d.message.help.filter(|h| !h.is_empty()) {
                                line.push_str(&format!(" ({help})"));
                            }
                            violations.push(line);
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

}

#[cfg(feature = "unit_technical_reviewer")]
pub use imp::run_type_lint_check;
