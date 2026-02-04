mod configs;
mod database;
mod libs;
mod middlewares;
mod models;
mod routes;
mod schema;
mod services;
use rustls::crypto::ring::default_provider;

#[actix_web::main]
async fn main() -> Result<(), actix_web::Error> {
    default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    routes::start_server().await?;

    Ok(())
}
