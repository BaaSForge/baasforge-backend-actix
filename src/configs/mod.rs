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
  pub postgres_host: String,
  pub postgres_port: String,
  pub postgres_user: String,
  pub postgres_password: String,
  pub postgres_db: String,
}

impl Settings {
  pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
    dotenv().ok();

    let app_env_str = env::var("APP_ENV").unwrap_or_else(|_| "staging".to_string());
    let app_environment = AppEnvironment::try_from(app_env_str)?;

    let port = env::var("PORT").expect("PORT must be set");
    let postgres_host = env::var("POSTGRES_HOST").expect("POSTGRES_HOST must be set");
    let postgres_port = env::var("POSTGRES_PORT").expect("POSTGRES_PORT must be set");
    let postgres_user = env::var("POSTGRES_USER").expect("POSTGRES_USER must be set");
    let postgres_password = env::var("POSTGRES_PASSWORD").expect("POSTGRES_PASSWORD must be set");
    let postgres_db = env::var("POSTGRES_DB").expect("POSTGRES_DB must be set");

    Ok(Settings {
      port,
      app_environment,
      postgres_host,
      postgres_port,
      postgres_user,
      postgres_password,
      postgres_db,
    })
  }
}
