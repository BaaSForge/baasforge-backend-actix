use actix_web;
mod configs;
mod routes;
mod libs;

#[actix_web::main]
async fn main() -> Result<(), actix_web::Error> {
  routes::start_server().await?;

  Ok(())
}
