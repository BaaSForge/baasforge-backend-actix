pub mod auth;
use crate::configs::Settings;
use crate::database::{DbPool, establish_pool};
use crate::middlewares::auth::{
    auth_refresh_token_maturity_guard_middleware, auth_closed_guard_middleware,
    auth_closed_refresh_token_guard_middleware, auth_opened_guard_middleware,
};
use actix_web::Result;
use actix_web::middleware::from_fn;
use actix_web::web;
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
    let port = settings
        .port
        .parse::<u16>()
        .expect("Failed to parse port number");

    let pool = establish_pool(settings.pg_database_url.clone());
    let app_context = AppContext::new(pool, settings.clone());

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(app_context.clone()))
            .service(
                web::scope("/api/v1/auth")
                    .route(
                        "/google/initiate",
                        web::post()
                            .to(auth::initiate_google_auth)
                            .wrap(from_fn(auth_opened_guard_middleware)),
                    )
                    .route(
                        "/google/callback",
                        web::post()
                            .to(auth::handle_google_auth_callback)
                            .wrap(from_fn(auth_opened_guard_middleware)),
                    )
                    .route(
                        "/logout",
                        web::post()
                            .to(auth::handle_logout)
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/refresh",
                        web::post()
                            .to(auth::handle_refresh_token)
                            .wrap(from_fn(auth_opened_guard_middleware))
                            .wrap(from_fn(auth_closed_refresh_token_guard_middleware)),
                    )
                    .route(
                        "/tokens",
                        web::delete()
                            .to(auth::handle_revoke_tokens)
                            .wrap(from_fn(auth_refresh_token_maturity_guard_middleware))
                            .wrap(from_fn(auth_closed_refresh_token_guard_middleware))
                            .wrap(from_fn(auth_closed_guard_middleware))
                    ),
            )
    })
    .bind(("127.0.0.1", port))?
    .run()
    .await?;

    Ok(())
}
