use crate::configs::{LlmProvider, Settings};
use crate::models::chat_messages::ChatSessionRoleType;
use redis::aio::MultiplexedConnection;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod openai;
pub mod gemini;
pub mod tools;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn payload_to_string(payload: &Option<Value>) -> String {
    match payload {
        Some(v) => v.as_str().map(String::from).unwrap_or_else(|| v.to_string()),
        None => String::new(),
    }
}

#[derive(Clone)]
pub enum LlmClient {
    OpenAi {
        credentials: ::openai::Credentials,
        model: String,
    },
    Gemini {
        api_key: String,
        model: String,
    },
}

impl LlmClient {
    pub async fn generate(
        &self,
        session_messages: &[(ChatSessionRoleType, Option<Value>)],
        user_text: &str,
        redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
        topic: &str,
    ) -> Result<String, BoxError> {
        match self {
            LlmClient::OpenAi { credentials, model } => {
                openai::call_gpt_stream(
                    credentials,
                    model,
                    session_messages,
                    user_text,
                    redis_conn,
                    topic,
                )
                .await
            }
            LlmClient::Gemini { api_key, model } => {
                gemini::call_gemini_stream(
                    api_key,
                    model,
                    session_messages,
                    user_text,
                    redis_conn,
                    topic,
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
                BoxError::from(
                    "LLM_PROVIDER=openai but OPENAI_API_KEY (or OPENAI_KEY) is not set",
                )
            })?;
            Ok(LlmClient::OpenAi {
                credentials: ::openai::Credentials::new(key, ""),
                model: settings.openai_model.clone(),
            })
        }
        LlmProvider::Gemini => {
            let key = settings.gemini_api_key.clone().ok_or_else(|| {
                BoxError::from("LLM_PROVIDER=gemini but GEMINI_API_KEY is not set")
            })?;
            Ok(LlmClient::Gemini {
                api_key: key,
                model: settings.gemini_model.clone(),
            })
        }
    }
}

