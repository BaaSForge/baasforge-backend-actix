pub mod auth;
pub mod chat;
pub mod package;
pub mod payment;
pub mod project;
pub mod purchase;
pub mod test;

use crate::configs::Settings;
use crate::database::{DbPool, establish_pool};
use crate::middlewares::auth::{
    auth_closed_guard_middleware, auth_closed_refresh_token_guard_middleware,
    auth_opened_guard_middleware, auth_refresh_token_maturity_guard_middleware,
};
use crate::middlewares::payment::paid_features_guard_middleware_factory;
use crate::middlewares::project::projects_owner_guard_middleware_factory;
use crate::middlewares::test::test_closed_stage_guard_middleware;
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
                web::scope("/api/v1/test")
                    .wrap(from_fn(test_closed_stage_guard_middleware))
                    .route(
                        "/first-cluster-pods",
                        web::get().to(test::handle_get_first_server_cluster),
                    )
                    .route(
                        "/first-cluster-nodes",
                        web::get().to(test::handle_get_first_server_cluster_nodes),
                    )
                    .route(
                        "/first-cluster-deploy",
                        web::post().to(test::handle_deploy_hello_world_pod),
                    )
                    .route("/rabbit-test", web::post().to(test::handle_rabbit_test))
                    .route(
                        "/chat-channel-send",
                        web::post().to(test::handle_chat_channel_send),
                    )
                    .route(
                        "/chat-channel-ws",
                        web::get().to(test::handle_serve_chat_channel_ws_page),
                    ),
            )
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
                        web::get()
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
                        web::get()
                            .to(auth::handle_list_tokens)
                            .wrap(from_fn(auth_closed_guard_middleware))
                            .wrap(from_fn(auth_closed_refresh_token_guard_middleware)),
                    )
                    .route(
                        "/tokens",
                        web::delete()
                            .to(auth::handle_revoke_tokens)
                            .wrap(from_fn(auth_refresh_token_maturity_guard_middleware))
                            .wrap(from_fn(auth_closed_refresh_token_guard_middleware))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/me",
                        web::get()
                            .to(auth::handle_get_current_user)
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    ),
            )
            .service(
                web::scope("/api/v1/payment")
                    .route("/packages", web::get().to(package::handle_list_packages))
                    .route(
                        "/purchase",
                        web::post()
                            .to(purchase::handle_initiate_purchase)
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/payments",
                        web::get()
                            .to(payment::handle_list_user_payments)
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/paid-packages",
                        web::get()
                            .to(payment::handle_get_user_paid_package)
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    ),
            )
            .service(
                web::scope("/api/v1/platform")
                    .route(
                        "/projects",
                        web::get()
                            .to(project::handle_list_projects)
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects",
                        web::post()
                            .to(project::handle_create_project)
                            .wrap(from_fn(paid_features_guard_middleware_factory(vec![])))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}",
                        web::delete()
                            .to(project::handle_delete_project)
                            .wrap(from_fn(auth_refresh_token_maturity_guard_middleware))
                            .wrap(from_fn(auth_closed_refresh_token_guard_middleware))
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}",
                        web::patch()
                            .to(project::handle_patch_project)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}",
                        web::get()
                            .to(project::handle_get_project)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/runtimes",
                        web::get()
                            .to(project::handle_list_project_runtimes)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/releases",
                        web::get()
                            .to(project::handle_list_project_releases)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/releases/{release_version}/units",
                        web::get()
                            .to(project::handle_list_project_release_units)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/units",
                        web::get()
                            .to(project::handle_list_project_units)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions",
                        web::post()
                            .to(chat::handle_create_project_chat_session)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions",
                        web::get()
                            .to(chat::handle_list_project_chat_sessions)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}",
                        web::get()
                            .to(chat::handle_get_project_chat_session)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/messages",
                        web::get()
                            .to(chat::handle_list_chat_session_messages)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/draft-units",
                        web::get()
                            .to(chat::handle_list_chat_session_draft_units)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/messages",
                        web::post()
                            .to(chat::handle_create_chat_session_message)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/attach-stage-runtime",
                        web::post()
                            .to(chat::handle_attach_chat_session_stage_runtime)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/detach-stage-runtime",
                        web::post()
                            .to(chat::handle_detach_chat_session_stage_runtime)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/channel",
                        web::get()
                            .to(chat::handle_chat_session_channel_ws)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    ),
            )
    })
    .bind(("127.0.0.1", port))?
    .run()
    .await?;

    Ok(())
}
