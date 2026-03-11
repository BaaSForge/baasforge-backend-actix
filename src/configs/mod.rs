use dotenv::dotenv;
use std::{env};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnvironment {
  Staging,
  Production,
}

impl Default for AppEnvironment {
  fn default() -> Self {
    AppEnvironment::Staging
  }
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
}

impl std::fmt::Display for LlmProvider {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      LlmProvider::OpenAi => write!(f, "openai"),
      LlmProvider::Gemini => write!(f, "gemini"),
    }
  }
}

impl TryFrom<String> for LlmProvider {
  type Error = String;

  fn try_from(s: String) -> Result<Self, Self::Error> {
    match s.to_lowercase().trim() {
      "openai" => Ok(LlmProvider::OpenAi),
      "gemini" => Ok(LlmProvider::Gemini),
      _ => Ok(LlmProvider::OpenAi),
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
  pub redis_url: Option<String>,
  pub openai_api_key: Option<String>,
  pub openai_model: String,
  pub gemini_api_key: Option<String>,
  pub gemini_model: String,
  pub llm_provider: LlmProvider,
}

impl Settings {
  pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
    dotenv().ok();

    let app_env_str = env::var("APP_ENV").unwrap_or_else(|_| "staging".to_string());
    let app_environment = AppEnvironment::try_from(app_env_str)?;

    let port = env::var("PORT").expect("PORT must be set");
    let pg_database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let database_pool_size = env::var("DATABASE_POOL_SIZE")
      .ok()
      .and_then(|s| s.parse().ok())
      .unwrap_or(2);
    let database_connection_timeout_secs = env::var("DATABASE_CONNECTION_TIMEOUT_SECS")
      .ok()
      .and_then(|s| s.parse().ok())
      .unwrap_or(30);
    let auth_access_token_secret = env::var("AUTH_ACCESS_TOKEN_SECRET").expect("AUTH_ACCESS_TOKEN_SECRET must be set");
    let auth_refresh_token_secret = env::var("AUTH_REFRESH_TOKEN_SECRET").expect("AUTH_REFRESH_TOKEN_SECRET must be set");
    let rabbit_url = env::var("RABBIT_URL").expect("RABBIT_URL must be set");
    let redis_url = env::var("REDIS_URL").ok();
    let openai_api_key = env::var("OPENAI_API_KEY").or_else(|_| env::var("OPENAI_KEY")).ok();
    let openai_model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let gemini_api_key = env::var("GEMINI_API_KEY").ok();
    let gemini_model = env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-flash-latest".to_string());
    let llm_provider_str = env::var("LLM_PROVIDER").unwrap_or_default();
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
      redis_url,
      openai_api_key,
      openai_model,
      gemini_api_key,
      gemini_model,
      llm_provider,
    })
  }
}
