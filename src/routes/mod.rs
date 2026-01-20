pub mod users;
use crate::configs::Settings;
use actix_web::web;
use actix_web::{Result};
use std::error::Error;

pub mod routes {
  use actix_web::{get, web};
  use crate::routes::Settings;

  #[get("/env")]
  pub async fn get_environment(settings: web::Data<Settings>) -> String {
    format!("App Environment: {:?}", settings.app_environment)
  }
}

pub async fn start_server() -> Result<(), Box<dyn Error>> {
  use actix_web::{App, HttpServer};

  let settings = Settings::new()?;

  let port = settings.port.parse::<u16>().expect("Failed to parse port number");

  HttpServer::new(move || {
    App::new()
      .app_data(web::Data::new(settings.clone()))
      .service(routes::get_environment)
      .service(users::get_user_token)
  })
  .bind(("127.0.0.1", port))?
  .run()
  .await?;

  Ok(())
}