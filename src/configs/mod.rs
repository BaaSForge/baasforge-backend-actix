use dotenv::dotenv;
use std::{env};

#[derive(Debug, Clone, Copy)]
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

#[derive(Debug, Clone)]
pub struct Settings {
  pub port: String,
  pub app_environment: AppEnvironment,
  pub pg_database_url: String,
  pub auth_access_token_secret: String,
  pub auth_refresh_token_secret: String,
}

impl Settings {
  pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
    dotenv().ok();

    let app_env_str = env::var("APP_ENV").unwrap_or_else(|_| "staging".to_string());
    let app_environment = AppEnvironment::try_from(app_env_str)?;

    let port = env::var("PORT").expect("PORT must be set");
    let pg_database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let auth_access_token_secret = env::var("AUTH_ACCESS_TOKEN_SECRET").expect("AUTH_ACCESS_TOKEN_SECRET must be set");
    let auth_refresh_token_secret = env::var("AUTH_REFRESH_TOKEN_SECRET").expect("AUTH_REFRESH_TOKEN_SECRET must be set");

    Ok(Settings {
      port,
      app_environment,
      pg_database_url,
      auth_access_token_secret,
      auth_refresh_token_secret,
    })
  }
}
