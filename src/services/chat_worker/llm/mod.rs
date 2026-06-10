use crate::configs::{LlmProvider, Settings};
use crate::database::DbPool;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageType, ChatSessionRoleType, TextPayload,
};
use crate::services::chat_worker::agent_flow::AgentFlowSession;
use crate::services::redis_conn::RedisConnection;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use tokio::sync::Mutex;
use uuid::Uuid;

pub mod claude;
pub mod gemini;
pub mod history;
pub mod openai;
pub mod tools;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

static LLM_SESSION_CALL_COUNTS: LazyLock<StdMutex<HashMap<Uuid, u32>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// Returns `Err` on call `threshold + 1` and later for the same `session_id` (each planner/tool continuation counts).
pub fn maybe_simulate_llm_api_error(session_id: Uuid, threshold: u32) -> Result<(), BoxError> {
    let mut counts = LLM_SESSION_CALL_COUNTS
        .lock()
        .map_err(|e| format!("llm simulate call counter lock: {}", e))?;
    let n = counts.entry(session_id).or_insert(0);
    *n += 1;
    let call_num = *n;
    drop(counts);
    if call_num > threshold {
        tracing::warn!(
            %session_id,
            call_num,
            threshold,
            "LLM_SIMULATE_ERROR_AFTER_CALLS: simulated LLM API failure"
        );
        return Err(format!(
            "simulated LLM API error after {} call(s) in this chat session (LLM_SIMULATE_ERROR_AFTER_CALLS={})",
            threshold, threshold
        )
        .into());
    }
    tracing::debug!(
        %session_id,
        call_num,
        threshold,
        "LLM call (simulate threshold not reached yet)"
    );
    Ok(())
}

/// Gemini thinking-related env (`GEMINI_THINKING_*`). See `.env` comments.
#[derive(Clone, Debug)]
pub struct GeminiThinkingEnv {
    pub explicitly_disabled: bool,
    pub level: Option<String>,
    pub budget: Option<i32>,
}

/// Result of LLM generation: either completed fully or stopped by user (partial text + optional tool calls).
#[derive(Debug)]
pub enum GenerateOutcome {
    Completed(TextPayload),
    Stopped(TextPayload),
}

#[derive(Clone)]
pub struct LlmClient {
    inner: LlmClientInner,
    /// `None` when simulation is disabled (`LLM_SIMULATE_ERROR_AFTER_CALLS` is `0`, `-1`, or unset).
    simulate_error_after_calls: Option<u32>,
}

#[derive(Clone)]
enum LlmClientInner {
    OpenAi {
        credentials: ::openai::Credentials,
        model: String,
        reasoning_effort: Option<String>,
        base_runtime_endpoint: String,
    },
    Gemini {
        api_key: String,
        model: String,
        thinking: GeminiThinkingEnv,
        base_runtime_endpoint: String,
    },
    Claude {
        api_key: String,
        model: String,
        thinking_budget_tokens: Option<u32>,
        base_runtime_endpoint: String,
    },
}

impl LlmClient {
    pub async fn generate(
        &self,
        pool: &DbPool,
        session_messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
        user_text: &str,
        redis_conn: Option<crate::services::redis_conn::RedisConn>,
        topic: &str,
        message_id: Uuid,
        user_id: Uuid,
        project_id: Uuid,
        session_id: Uuid,
        agent: ChatAgentType,
        queue_invoking_message_id: Option<Uuid>,
        stale_base_release: bool,
        flow: AgentFlowSession,
    ) -> Result<GenerateOutcome, BoxError> {
        if let Some(threshold) = self.simulate_error_after_calls {
            maybe_simulate_llm_api_error(session_id, threshold)?;
        }

        match &self.inner {
            LlmClientInner::OpenAi {
                credentials,
                model,
                reasoning_effort,
                base_runtime_endpoint,
            } => {
                openai::call_gpt_stream(
                    credentials,
                    model,
                    reasoning_effort.as_deref(),
                    pool,
                    session_messages,
                    user_text,
                    redis_conn,
                    topic,
                    message_id,
                    user_id,
                    project_id,
                    session_id,
                    agent,
                    queue_invoking_message_id,
                    stale_base_release,
                    base_runtime_endpoint,
                    flow,
                )
                .await
            }
            LlmClientInner::Gemini {
                api_key,
                model,
                thinking,
                base_runtime_endpoint,
            } => {
                gemini::call_gemini_stream(
                    api_key,
                    model,
                    thinking,
                    pool,
                    session_messages,
                    user_text,
                    redis_conn,
                    topic,
                    message_id,
                    user_id,
                    project_id,
                    session_id,
                    agent,
                    queue_invoking_message_id,
                    stale_base_release,
                    base_runtime_endpoint,
                    flow,
                )
                .await
            }
            LlmClientInner::Claude {
                api_key,
                model,
                thinking_budget_tokens,
                base_runtime_endpoint,
            } => {
                claude::call_claude_stream(
                    api_key,
                    model,
                    *thinking_budget_tokens,
                    pool,
                    session_messages,
                    user_text,
                    redis_conn,
                    topic,
                    message_id,
                    user_id,
                    project_id,
                    session_id,
                    agent,
                    queue_invoking_message_id,
                    stale_base_release,
                    base_runtime_endpoint,
                    flow,
                )
                .await
            }
        }
    }
}

pub fn build_llm_client(settings: &Settings) -> Result<LlmClient, BoxError> {
    let simulate_error_after_calls = settings.llm_simulate_error_after_calls;
    let provider = settings.llm_provider;
    let inner = match provider {
        LlmProvider::OpenAi => {
            let key = settings.openai_api_key.as_deref().ok_or_else(|| {
                BoxError::from("LLM_PROVIDER=openai but OPENAI_API_KEY (or OPENAI_KEY) is not set")
            })?;
            LlmClientInner::OpenAi {
                credentials: ::openai::Credentials::new(key, ""),
                model: settings.openai_model.clone(),
                reasoning_effort: settings.openai_reasoning_effort.clone(),
                base_runtime_endpoint: settings.base_runtime_endpoint.clone(),
            }
        }
        LlmProvider::Gemini => {
            let key = settings.gemini_api_key.clone().ok_or_else(|| {
                BoxError::from("LLM_PROVIDER=gemini but GEMINI_API_KEY is not set")
            })?;
            LlmClientInner::Gemini {
                api_key: key,
                model: settings.gemini_model.clone(),
                thinking: GeminiThinkingEnv {
                    explicitly_disabled: settings.gemini_thinking_explicitly_disabled,
                    level: settings.gemini_thinking_level.clone(),
                    budget: settings.gemini_thinking_budget,
                },
                base_runtime_endpoint: settings.base_runtime_endpoint.clone(),
            }
        }
        LlmProvider::Anthropic => {
            let key = settings.anthropic_api_key.clone().ok_or_else(|| {
                BoxError::from("LLM_PROVIDER=anthropic but ANTHROPIC_API_KEY is not set")
            })?;
            LlmClientInner::Claude {
                api_key: key,
                model: settings.anthropic_model.clone(),
                thinking_budget_tokens: settings.anthropic_thinking_budget_tokens,
                base_runtime_endpoint: settings.base_runtime_endpoint.clone(),
            }
        }
    };
    Ok(LlmClient {
        inner,
        simulate_error_after_calls,
    })
}
