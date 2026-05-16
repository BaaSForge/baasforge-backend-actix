//! Redact project runtime secret values from Jaeger trace JSON before persistence or API responses.

use crate::models::runtime_env_vars::ProjectRuntimeEnvVarRow;
use crate::schema::project_runtime_encryption_keys;
use crate::schema::project_runtime_env_vars;
use crate::services::runtime_secrets::{
    RuntimeSecretsError, decrypt_runtime_env_value, runtime_key_from_row,
};
use diesel::prelude::*;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use thiserror::Error;
use uuid::Uuid;

pub const MIN_MASKABLE_SECRET_LEN: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceMaskSecret {
    pub env_key: String,
    pub value: String,
}

#[derive(Debug, Error)]
pub enum TraceSecretsMaskingError {
    #[error("database error")]
    Database(#[from] diesel::result::Error),
    #[error("runtime secrets error")]
    RuntimeSecrets(#[from] RuntimeSecretsError),
}

/// Loads encrypted runtime env vars for `project_id`, decrypts values, and returns mask rules.
pub fn load_project_trace_mask_secrets(
    conn: &mut diesel::PgConnection,
    project_id: Uuid,
) -> Result<Vec<TraceMaskSecret>, TraceSecretsMaskingError> {
    let rows: Vec<ProjectRuntimeEnvVarRow> = project_runtime_env_vars::table
        .filter(project_runtime_env_vars::project_id.eq(project_id))
        .filter(project_runtime_env_vars::is_encrypted.eq(true))
        .select(ProjectRuntimeEnvVarRow::as_select())
        .load(conn)?;

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let runtime_ids: Vec<Uuid> = rows
        .iter()
        .map(|r| r.runtime_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let key_rows = project_runtime_encryption_keys::table
        .filter(project_runtime_encryption_keys::runtime_id.eq_any(&runtime_ids))
        .load::<crate::models::runtime_env_vars::ProjectRuntimeEncryptionKeyRow>(conn)?;

    let mut runtime_keys: HashMap<Uuid, [u8; 32]> = HashMap::new();
    for row in key_rows {
        runtime_keys.insert(row.runtime_id, runtime_key_from_row(&row)?);
    }

    let mut secrets = Vec::new();
    let mut seen_values: HashSet<String> = HashSet::new();

    for row in rows {
        let Some(runtime_key) = runtime_keys.get(&row.runtime_id) else {
            log::warn!(
                "[trace_secrets_masking] missing encryption key runtime_id={} env_key={}",
                row.runtime_id,
                row.key
            );
            continue;
        };
        let plaintext = match decrypt_runtime_env_value(runtime_key, &row.stored_value) {
            Ok(v) => v,
            Err(e) => {
                log::warn!(
                    "[trace_secrets_masking] decrypt failed runtime_id={} env_key={}: {}",
                    row.runtime_id,
                    row.key,
                    e
                );
                continue;
            }
        };
        if plaintext.len() < MIN_MASKABLE_SECRET_LEN {
            continue;
        }
        if !seen_values.insert(plaintext.clone()) {
            continue;
        }
        secrets.push(TraceMaskSecret {
            env_key: row.key,
            value: plaintext,
        });
    }

    secrets.sort_by(|a, b| b.value.len().cmp(&a.value.len()));
    Ok(secrets)
}

pub fn load_project_trace_mask_secrets_diesel(
    conn: &mut diesel::PgConnection,
    project_id: Uuid,
) -> Result<Vec<TraceMaskSecret>, diesel::result::Error> {
    load_project_trace_mask_secrets(conn, project_id)
        .map_err(|e| diesel::result::Error::QueryBuilderError(Box::new(e)))
}

pub fn mask_token(env_key: &str) -> String {
    format!("[MASKED:{env_key}]")
}

pub fn mask_string(input: &str, secrets: &[TraceMaskSecret]) -> String {
    let mut out = input.to_string();
    for secret in secrets {
        if secret.value.len() < MIN_MASKABLE_SECRET_LEN {
            continue;
        }
        let token = mask_token(&secret.env_key);
        if out.contains(&secret.value) {
            out = out.replace(&secret.value, &token);
        }
    }
    out
}

pub fn mask_trace_json(value: Value, secrets: &[TraceMaskSecret]) -> Value {
    if secrets.is_empty() {
        return value;
    }
    match value {
        Value::String(s) => Value::String(mask_string(&s, secrets)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| mask_trace_json(v, secrets))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let masked_key = mask_string(&k, secrets);
                out.insert(masked_key, mask_trace_json(v, secrets));
            }
            Value::Object(out)
        }
        Value::Number(n) => {
            let raw = n.to_string();
            let masked = mask_string(&raw, secrets);
            if masked == raw {
                Value::Number(n)
            } else {
                Value::String(masked)
            }
        }
        Value::Bool(b) => {
            let raw = b.to_string();
            let masked = mask_string(&raw, secrets);
            if masked == raw {
                Value::Bool(b)
            } else {
                Value::String(masked)
            }
        }
        other => other,
    }
}

pub fn mask_jaeger_spans(spans: &mut [Value], secrets: &[TraceMaskSecret]) {
    if secrets.is_empty() {
        return;
    }
    for span in spans.iter_mut() {
        *span = mask_trace_json(std::mem::take(span), secrets);
    }
}

pub fn mask_jaeger_by_service(
    by_service: &mut BTreeMap<String, Vec<Value>>,
    secrets: &[TraceMaskSecret],
) {
    if secrets.is_empty() {
        return;
    }
    for spans in by_service.values_mut() {
        mask_jaeger_spans(spans, secrets);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn secrets(pairs: &[(&str, &str)]) -> Vec<TraceMaskSecret> {
        let mut out: Vec<TraceMaskSecret> = pairs
            .iter()
            .map(|(k, v)| TraceMaskSecret {
                env_key: (*k).to_string(),
                value: (*v).to_string(),
            })
            .collect();
        out.sort_by(|a, b| b.value.len().cmp(&a.value.len()));
        out
    }

    #[test]
    fn masks_string_fields() {
        let s = secrets(&[("API_KEY", "sk-live-abcdef")]);
        let input = json!({ "message": "auth=sk-live-abcdef ok" });
        let out = mask_trace_json(input, &s);
        assert_eq!(
            out["message"],
            "auth=[MASKED:API_KEY] ok"
        );
    }

    #[test]
    fn masks_object_keys_containing_secrets() {
        let s = secrets(&[("DB_PASS", "hunter2secret")]);
        let input = json!({ "hunter2secret_field": 1 });
        let out = mask_trace_json(input, &s);
        assert!(out.as_object().unwrap().contains_key("[MASKED:DB_PASS]_field"));
    }

    #[test]
    fn longest_secret_masked_first() {
        let s = secrets(&[
            ("SHORT", "abcdef"),
            ("LONG", "abcdef-extra-tail"),
        ]);
        let input = json!("prefix abcdef-extra-tail suffix");
        let out = mask_trace_json(input, &s);
        assert_eq!(out, "prefix [MASKED:LONG] suffix");
    }

    #[test]
    fn skips_short_secrets() {
        let s = secrets(&[("TINY", "abc")]);
        let input = json!("abc");
        let out = mask_trace_json(input, &s);
        assert_eq!(out, "abc");
    }

    #[test]
    fn masks_nested_tags_array() {
        let s = secrets(&[("TOKEN", "super-secret-token")]);
        let input = json!({
            "tags": [
                { "key": "http.url", "value": "https://x?token=super-secret-token" }
            ]
        });
        let out = mask_trace_json(input, &s);
        assert_eq!(
            out["tags"][0]["value"],
            "https://x?token=[MASKED:TOKEN]"
        );
    }

    #[test]
    fn masks_number_when_value_matches() {
        let s = secrets(&[("PIN", "42424242")]);
        let input = json!({ "code": 42424242 });
        let out = mask_trace_json(input, &s);
        assert_eq!(out["code"], "[MASKED:PIN]");
    }

}
