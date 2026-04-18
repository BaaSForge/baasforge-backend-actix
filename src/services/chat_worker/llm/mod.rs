use crate::configs::{LlmProvider, Settings};
use crate::database::DbPool;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageType, ChatSessionRoleType, TextPayload,
};
use redis::aio::MultiplexedConnection;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub mod claude;
pub mod gemini;
pub mod history;
pub mod openai;
pub mod tools;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

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
pub enum LlmClient {
    OpenAi {
        credentials: ::openai::Credentials,
        model: String,
        /// Chat Completions `reasoning_effort` when set (reasoning-capable models only).
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
        /// `None` disables extended thinking on the request.
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
        redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
        topic: &str,
        message_id: Uuid,
        user_id: Uuid,
        project_id: Uuid,
        session_id: Uuid,
        agent: ChatAgentType,
        queue_invoking_message_id: Option<Uuid>,
        stale_base_release: bool,
    ) -> Result<GenerateOutcome, BoxError> {
        match self {
            LlmClient::OpenAi {
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
                )
                .await
            }
            LlmClient::Gemini {
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
                )
                .await
            }
            LlmClient::Claude {
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
                )
                .await
            }
        }
    }
}

pub fn build_llm_client(settings: &Settings) -> Result<LlmClient, BoxError> {
    let provider = settings.llm_provider;
    match provider {
        LlmProvider::OpenAi => {
            let key = settings.openai_api_key.as_deref().ok_or_else(|| {
                BoxError::from("LLM_PROVIDER=openai but OPENAI_API_KEY (or OPENAI_KEY) is not set")
            })?;
            Ok(LlmClient::OpenAi {
                credentials: ::openai::Credentials::new(key, ""),
                model: settings.openai_model.clone(),
                reasoning_effort: settings.openai_reasoning_effort.clone(),
                base_runtime_endpoint: settings.base_runtime_endpoint.clone(),
            })
        }
        LlmProvider::Gemini => {
            let key = settings.gemini_api_key.clone().ok_or_else(|| {
                BoxError::from("LLM_PROVIDER=gemini but GEMINI_API_KEY is not set")
            })?;
            Ok(LlmClient::Gemini {
                api_key: key,
                model: settings.gemini_model.clone(),
                thinking: GeminiThinkingEnv {
                    explicitly_disabled: settings.gemini_thinking_explicitly_disabled,
                    level: settings.gemini_thinking_level.clone(),
                    budget: settings.gemini_thinking_budget,
                },
                base_runtime_endpoint: settings.base_runtime_endpoint.clone(),
            })
        }
        LlmProvider::Anthropic => {
            let key = settings.anthropic_api_key.clone().ok_or_else(|| {
                BoxError::from("LLM_PROVIDER=anthropic but ANTHROPIC_API_KEY is not set")
            })?;
            Ok(LlmClient::Claude {
                api_key: key,
                model: settings.anthropic_model.clone(),
                thinking_budget_tokens: settings.anthropic_thinking_budget_tokens,
                base_runtime_endpoint: settings.base_runtime_endpoint.clone(),
            })
        }
    }
}
