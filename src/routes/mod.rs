pub mod auth;
use crate::configs::Settings;
use crate::database::{DbPool, establish_pool};
use actix_web::web;
use actix_web::{Result};
use std::error::Error;

#[derive(Clone)]
pub struct AppContext {
  pub pool: DbPool,
  pub settings: Settings,
}

impl AppContext {
  pub fn new(pool: DbPool, settings: Settings) -> Self {
    Self { pool, settings }
  }
}

pub async fn start_server() -> Result<(), Box<dyn Error>> {
  use actix_web::{App, HttpServer};

  let settings = Settings::new()?;
  let port = settings.port.parse::<u16>().expect("Failed to parse port number");

  let pool = establish_pool(settings.pg_database_url.clone());
  let app_context = AppContext::new(pool, settings.clone());

  HttpServer::new(move || {
    App::new()
      .app_data(web::Data::new(app_context.clone()))
      .service(
        web::scope("/api/v1")
          .service(auth::initiate_google_auth)
          .service(auth::handle_google_auth_callback),
      )
  })
  .bind(("127.0.0.1", port))?
  .run()
  .await?;

  Ok(())
}