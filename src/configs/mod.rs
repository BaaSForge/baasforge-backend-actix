//! Application configuration loaded from environment variables.
//! All settings must be set via .env (or the environment); there are no defaults.
//! Load order: .env first (base), then .env.local overrides if present.

use dotenvy::from_filename;
use dotenvy::from_filename_override;
use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppEnvironment {
    #[default]
    Staging,
    Production,
}

impl TryFrom<String> for AppEnvironment {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.to_lowercase().as_str() {
            "staging" => Ok(AppEnvironment::Staging),
            "production" => Ok(AppEnvironment::Production),
            _ => Err("Invalid APP_ENV. Must be 'staging' or 'production'".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    OpenAi,
    Gemini,
    Anthropic,
}

impl std::fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmProvider::OpenAi => write!(f, "openai"),
            LlmProvider::Gemini => write!(f, "gemini"),
            LlmProvider::Anthropic => write!(f, "anthropic"),
        }
    }
}

impl TryFrom<String> for LlmProvider {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.to_lowercase().trim() {
            "openai" => Ok(LlmProvider::OpenAi),
            "gemini" => Ok(LlmProvider::Gemini),
            "anthropic" => Ok(LlmProvider::Anthropic),
            _ => {
                Err("Invalid LLM_PROVIDER. Must be 'openai', 'gemini', or 'anthropic'".to_string())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub port: String,
    pub app_environment: AppEnvironment,
    pub pg_database_url: String,
    /// Per-process pool size. Total DB connections = (API_replicas + worker_replicas) × this. Keep under PostgreSQL max_connections.
    pub database_pool_size: u32,
    pub database_connection_timeout_secs: u64,
    pub auth_access_token_secret: String,
    pub auth_refresh_token_secret: String,
    pub rabbit_url: String,
    /// Exchange used for retry delay: failed messages are published here, then after TTL are dead-lettered back to the original queue.
    pub retry_delay_exchange: String,
    /// TTL in milliseconds for messages in retry delay queues (e.g. 3000 = 3 seconds).
    pub retry_delay_ttl_ms: u32,
    /// Chat session queue: if a consumer doesn't ack within this time (seconds), RabbitMQ closes the channel and requeues (e.g. after crash). Used by queue_setup_http.sh for `x-consumer-timeout`.
    /// Size this for **one** queue message (one LLM pass + retrievals for that step); tool **executions** are separate messages—see `docs/chat-queue.md`.
    pub chat_consumer_timeout_secs: u64,
    /// TTL in seconds for Redis keys that mark stopped message IDs (e.g. 600 = 10 minutes). Used when user stops generating so workers can skip or abort streaming for those messages.
    pub stopped_messages_ids_cache_ttl_secs: u64,
    pub redis_url: String,
    pub openai_api_key: Option<String>,
    pub openai_model: String,
    /// Chat Completions: `reasoning_effort` for reasoning models (e.g. gpt-5.x, o3, o4-mini). See .env comments.
    pub openai_reasoning_effort: Option<String>,
    pub gemini_api_key: Option<String>,
    pub gemini_model: String,
    pub anthropic_api_key: Option<String>,
    pub anthropic_model: String,
    /// Anthropic extended thinking: present only when enabled (see `ANTHROPIC_THINKING_ENABLED`) and
    /// `ANTHROPIC_THINKING_BUDGET_TOKENS` is > 0. Passed to the Messages API as `thinking.budget_tokens`.
    pub anthropic_thinking_budget_tokens: Option<u32>,
    pub llm_provider: LlmProvider,
}

impl Settings {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        from_filename(".env").ok();
        from_filename_override(".env.local").ok();

        let app_env_str = env::var("APP_ENV").expect("APP_ENV must be set");
        let app_environment = AppEnvironment::try_from(app_env_str)?;

        let port = env::var("PORT").expect("PORT must be set");
        let pg_database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let database_pool_size: u32 = env::var("DATABASE_POOL_SIZE")
            .expect("DATABASE_POOL_SIZE must be set")
            .parse()
            .expect("DATABASE_POOL_SIZE must be a valid u32");
        let database_connection_timeout_secs: u64 = env::var("DATABASE_CONNECTION_TIMEOUT_SECS")
            .expect("DATABASE_CONNECTION_TIMEOUT_SECS must be set")
            .parse()
            .expect("DATABASE_CONNECTION_TIMEOUT_SECS must be a valid u64");
        let auth_access_token_secret =
            env::var("AUTH_ACCESS_TOKEN_SECRET").expect("AUTH_ACCESS_TOKEN_SECRET must be set");
        let auth_refresh_token_secret =
            env::var("AUTH_REFRESH_TOKEN_SECRET").expect("AUTH_REFRESH_TOKEN_SECRET must be set");
        let rabbit_url = env::var("RABBIT_URL").expect("RABBIT_URL must be set");
        let retry_delay_exchange =
            env::var("RETRY_DELAY_EXCHANGE").expect("RETRY_DELAY_EXCHANGE must be set");
        let retry_delay_ttl_ms: u32 = env::var("RETRY_DELAY_TTL_MS")
            .expect("RETRY_DELAY_TTL_MS must be set")
            .parse()
            .expect("RETRY_DELAY_TTL_MS must be a valid u32");
        let chat_consumer_timeout_secs: u64 = env::var("CHAT_CONSUMER_TIMEOUT_SECS")
            .expect("CHAT_CONSUMER_TIMEOUT_SECS must be set")
            .parse()
            .expect("CHAT_CONSUMER_TIMEOUT_SECS must be a valid u64");
        let stopped_messages_ids_cache_ttl_secs: u64 =
            env::var("STOPPED_MESSAGES_IDS_CACHE_TTL_SECS")
                .expect("STOPPED_MESSAGES_IDS_CACHE_TTL_SECS must be set")
                .parse()
                .expect("STOPPED_MESSAGES_IDS_CACHE_TTL_SECS must be a valid u64");
        let redis_url = env::var("REDIS_URL").expect("REDIS_URL must be set");
        let openai_api_key = env::var("OPENAI_API_KEY")
            .or_else(|_| env::var("OPENAI_KEY"))
            .ok();
        let openai_model = env::var("OPENAI_MODEL").expect("OPENAI_MODEL must be set");
        let openai_reasoning_effort: Option<String> = env::var("OPENAI_REASONING_EFFORT")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let gemini_api_key = env::var("GEMINI_API_KEY").ok();
        let gemini_model = env::var("GEMINI_MODEL").expect("GEMINI_MODEL must be set");
        let anthropic_api_key = env::var("ANTHROPIC_API_KEY").ok();
        let anthropic_model = env::var("ANTHROPIC_MODEL").expect("ANTHROPIC_MODEL must be set");
        let anthropic_thinking_enabled_flag: Option<bool> =
            match env::var("ANTHROPIC_THINKING_ENABLED") {
                Ok(s) if s.trim().is_empty() => None,
                Ok(s) => Some(
                    parse_env_bool(s.trim()).map_err(|msg| {
                        format!("ANTHROPIC_THINKING_ENABLED: {}", msg)
                    })?,
                ),
                Err(_) => None,
            };
        let anthropic_thinking_budget_raw: Option<u32> =
            env::var("ANTHROPIC_THINKING_BUDGET_TOKENS")
                .ok()
                .and_then(|s| s.parse().ok())
                .filter(|&n| n > 0);
        let anthropic_thinking_budget_tokens: Option<u32> = match anthropic_thinking_enabled_flag {
            Some(false) => None,
            Some(true) | None => anthropic_thinking_budget_raw,
        };
        let llm_provider_str = env::var("LLM_PROVIDER").expect("LLM_PROVIDER must be set");
        let llm_provider = LlmProvider::try_from(llm_provider_str)?;

        Ok(Settings {
            port,
            app_environment,
            pg_database_url,
            database_pool_size,
            database_connection_timeout_secs,
            auth_access_token_secret,
            auth_refresh_token_secret,
            rabbit_url,
            retry_delay_exchange,
            retry_delay_ttl_ms,
            chat_consumer_timeout_secs,
            stopped_messages_ids_cache_ttl_secs,
            redis_url,
            openai_api_key,
            openai_model,
            openai_reasoning_effort,
            gemini_api_key,
            gemini_model,
            anthropic_api_key,
            anthropic_model,
            anthropic_thinking_budget_tokens,
            llm_provider,
        })
    }
}

/// Parses typical env truthy/falsey strings (e.g. `true`, `1`, `on`, `false`, `0`, `off`).
fn parse_env_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(format!(
            "expected true/false, 1/0, yes/no, or on/off; got {:?}",
            other
        )),
    }
}
