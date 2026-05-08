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
    /// `GEMINI_THINKING_ENABLED=false` disables thinking even if level/budget are set.
    pub gemini_thinking_explicitly_disabled: bool,
    /// Gemini 3: `minimal` / `low` / `medium` / `high` (mutually exclusive with budget in the API).
    pub gemini_thinking_level: Option<String>,
    /// Gemini 2.5: token budget; `-1` = dynamic. Omit env var to leave thinking off (unless level is set).
    pub gemini_thinking_budget: Option<i32>,
    pub anthropic_api_key: Option<String>,
    pub anthropic_model: String,
    /// Anthropic extended thinking: present only when enabled (see `ANTHROPIC_THINKING_ENABLED`) and
    /// `ANTHROPIC_THINKING_BUDGET_TOKENS` is > 0. Passed to the Messages API as `thinking.budget_tokens`.
    pub anthropic_thinking_budget_tokens: Option<u32>,
    pub llm_provider: LlmProvider,
    /// When set, the API connects to NATS on startup and publishes release events.
    pub runtime_nats_url: Option<String>,
    /// Subject for JetStream release publishes (must match the stream’s subject list).
    pub runtime_nats_subject_release: String,
    /// JetStream stream name (listener creates the stream; publisher uses JetStream publish).
    pub runtime_nats_jetstream_stream_release: String,
    /// JetStream consumer name on that stream (used by the release listener).
    pub runtime_nats_jetstream_consumer_release: String,
    /// When `true`, every unit reviewer processing attempt returns an error immediately,
    /// exercising the full retry → dead-letter → fail_tool_message_and_continue path.
    /// Set `UNIT_REVIEWER_SIMULATE_FAILURE=true` in .env.local; absent or `false` disables it.
    pub unit_reviewer_simulate_failure: bool,
    /// Base URL of the runtime, no trailing slash (e.g. `https://runtime.example.com`).
    /// Injected into agent prompts and returned per unit type in `GET .../platform/projects/{id}` (`runtimes`).
    pub base_runtime_endpoint: String,
    /// Jaeger Query base URL, no trailing slash (e.g. `http://localhost:16686`).
    /// Used by the observability proxy endpoint to fetch traces by traceID.
    pub jaeger_query_base_url: String,
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
        let gemini_thinking_enabled_flag: Option<bool> = match env::var("GEMINI_THINKING_ENABLED") {
            Ok(s) if s.trim().is_empty() => None,
            Ok(s) => match parse_env_bool(s.trim()) {
                Ok(b) => Some(b),
                Err(msg) => {
                    return Err(format!("GEMINI_THINKING_ENABLED: {}", msg).into());
                }
            },
            Err(_) => None,
        };
        let gemini_thinking_explicitly_disabled = gemini_thinking_enabled_flag == Some(false);
        let gemini_thinking_level = env::var("GEMINI_THINKING_LEVEL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let gemini_thinking_budget = env::var("GEMINI_THINKING_BUDGET").ok().and_then(|s| {
            let t = s.trim();
            if t.is_empty() { None } else { t.parse().ok() }
        });
        let anthropic_api_key = env::var("ANTHROPIC_API_KEY").ok();
        let anthropic_model = env::var("ANTHROPIC_MODEL").expect("ANTHROPIC_MODEL must be set");
        let anthropic_thinking_enabled_flag: Option<bool> =
            match env::var("ANTHROPIC_THINKING_ENABLED") {
                Ok(s) if s.trim().is_empty() => None,
                Ok(s) => Some(
                    parse_env_bool(s.trim())
                        .map_err(|msg| format!("ANTHROPIC_THINKING_ENABLED: {}", msg))?,
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

        let runtime_nats_url = env::var("RUNTIME_NATS_URL").ok().and_then(|s| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        });
        let runtime_nats_subject_release_raw = env::var("RUNTIME_NATS_SUBJECT_RELEASE")
            .expect("RUNTIME_NATS_SUBJECT_RELEASE must be set");
        let runtime_nats_subject_release = runtime_nats_subject_release_raw.trim();
        if runtime_nats_subject_release.is_empty() {
            return Err("RUNTIME_NATS_SUBJECT_RELEASE must not be empty".into());
        }
        let runtime_nats_subject_release = runtime_nats_subject_release.to_string();

        let runtime_nats_jetstream_stream_release_raw =
            env::var("RUNTIME_NATS_JETSTREAM_STREAM_RELEASE")
                .expect("RUNTIME_NATS_JETSTREAM_STREAM_RELEASE must be set");
        let runtime_nats_jetstream_stream_release =
            runtime_nats_jetstream_stream_release_raw.trim();
        if runtime_nats_jetstream_stream_release.is_empty() {
            return Err("RUNTIME_NATS_JETSTREAM_STREAM_RELEASE must not be empty".into());
        }
        let runtime_nats_jetstream_stream_release =
            runtime_nats_jetstream_stream_release.to_string();

        let runtime_nats_jetstream_consumer_release_raw =
            env::var("RUNTIME_NATS_JETSTREAM_CONSUMER_RELEASE")
                .expect("RUNTIME_NATS_JETSTREAM_CONSUMER_RELEASE must be set");
        let runtime_nats_jetstream_consumer_release =
            runtime_nats_jetstream_consumer_release_raw.trim();
        if runtime_nats_jetstream_consumer_release.is_empty() {
            return Err("RUNTIME_NATS_JETSTREAM_CONSUMER_RELEASE must not be empty".into());
        }
        let runtime_nats_jetstream_consumer_release =
            runtime_nats_jetstream_consumer_release.to_string();

        let unit_reviewer_simulate_failure: bool = match env::var("UNIT_REVIEWER_SIMULATE_FAILURE")
        {
            Ok(s) if s.trim().is_empty() => false,
            Ok(s) => parse_env_bool(s.trim())
                .map_err(|msg| format!("UNIT_REVIEWER_SIMULATE_FAILURE: {}", msg))?,
            Err(_) => false,
        };

        let base_runtime_endpoint = env::var("BASE_RUNTIME_ENDPOINT")
            .expect("BASE_RUNTIME_ENDPOINT must be set")
            .trim_end_matches('/')
            .to_string();

        let jaeger_query_base_url = env::var("JAEGER_QUERY_BASE_URL")
            .expect("JAEGER_QUERY_BASE_URL must be set")
            .trim_end_matches('/')
            .to_string();
        if jaeger_query_base_url.is_empty() {
            return Err("JAEGER_QUERY_BASE_URL must not be empty".into());
        }

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
            gemini_thinking_explicitly_disabled,
            gemini_thinking_level,
            gemini_thinking_budget,
            anthropic_api_key,
            anthropic_model,
            anthropic_thinking_budget_tokens,
            llm_provider,
            runtime_nats_url,
            runtime_nats_subject_release,
            runtime_nats_jetstream_stream_release,
            runtime_nats_jetstream_consumer_release,
            unit_reviewer_simulate_failure,
            base_runtime_endpoint,
            jaeger_query_base_url,
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
