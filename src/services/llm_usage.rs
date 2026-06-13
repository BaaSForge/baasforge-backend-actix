use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::libs::ids::new_uuid;
use crate::services::usage_publisher::{self, UsageEntry};

#[derive(Debug, Clone)]
pub struct LlmUsageRecord {
    pub project_id: Uuid,
    pub model: String,
    pub provider: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub request_id: Option<String>,
    pub chat_session_id: Option<Uuid>,
    pub message_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub cached_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
}

impl UsageEntry {
    fn from_llm_tokens(record: &LlmUsageRecord, metric: &str, quantity: u64) -> Option<Self> {
        if quantity == 0 {
            return None;
        }
        let mut details = json!({
            "model": record.model,
            "provider": record.provider,
            "request_id": record.request_id,
            "chat_session_id": record.chat_session_id,
            "input_tokens": record.input_tokens,
            "output_tokens": record.output_tokens,
        });
        if let Some(message_id) = record.message_id {
            details["message_id"] = json!(message_id);
        }
        if let Some(user_id) = record.user_id {
            details["user_id"] = json!(user_id);
        }
        if let Some(cached) = record.cached_tokens {
            details["cached_tokens"] = json!(cached);
        }
        if let Some(reasoning) = record.reasoning_tokens {
            details["reasoning_tokens"] = json!(reasoning);
        }
        Some(Self {
            id: new_uuid(),
            project_id: record.project_id,
            recorded_at: Utc::now(),
            kind: "llm".to_string(),
            metric: metric.to_string(),
            quantity: i64::try_from(quantity).unwrap_or(i64::MAX),
            price_unit: "token".to_string(),
            details,
        })
    }
}

pub fn log_llm_usage(record: LlmUsageRecord) {
    tracing::info!(
        target: "baal.usage.llm",
        project_id = %record.project_id,
        model = %record.model,
        provider = %record.provider,
        input_tokens = record.input_tokens,
        output_tokens = record.output_tokens,
        request_id = record.request_id.as_deref().unwrap_or(""),
        chat_session_id = ?record.chat_session_id,
        "llm usage"
    );
    if let Some(entry) = UsageEntry::from_llm_tokens(&record, "input_tokens", record.input_tokens) {
        usage_publisher::enqueue(entry);
    }
    if let Some(entry) = UsageEntry::from_llm_tokens(&record, "output_tokens", record.output_tokens)
    {
        usage_publisher::enqueue(entry);
    }
}
