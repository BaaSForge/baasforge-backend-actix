pub mod auth;
pub mod chat;
pub mod observability;
pub mod package;
pub mod platform;
pub mod payment;
pub mod project;
pub mod runtime_env_vars;
pub mod purchase;
pub mod test;
pub mod ws;

use crate::configs::Settings;
use crate::database::{DbPool, establish_pool};
use crate::middlewares::auth::{
    auth_closed_guard_middleware, auth_closed_refresh_token_guard_middleware,
    auth_opened_guard_middleware, auth_refresh_token_maturity_guard_middleware,
};
use crate::middlewares::cors::cors_middleware;
use crate::middlewares::payment::paid_features_guard_middleware_factory;
use crate::middlewares::project::projects_owner_guard_middleware_factory;
use crate::middlewares::test::test_closed_stage_guard_middleware;
use crate::routes::chat::message_queue;
use crate::services::message_queue as mq_service;
use crate::services::runtime_nats::RuntimeNatsPublisher;
use actix_web::Result;
use actix_web::middleware::from_fn;
use actix_web::web;
use std::error::Error;
use std::sync::Arc;
use tokio::sync::Mutex;

pub type SharedRedisConn = Option<Arc<Mutex<redis::aio::MultiplexedConnection>>>;

pub type SharedRuntimeNatsPublisher = Option<Arc<RuntimeNatsPublisher>>;

#[derive(Clone)]
pub struct AppContext {
    pub pool: DbPool,
    pub settings: Settings,
    pub ws_broker: Arc<ws::WsBroker>,
    pub rabbit_publisher: Option<Arc<mq_service::SharedRabbitPublisher>>,
    pub redis_conn: SharedRedisConn,
    pub runtime_nats_publisher: SharedRuntimeNatsPublisher,
}

impl AppContext {
    pub fn new(
        pool: DbPool,
        settings: Settings,
        ws_broker: Arc<ws::WsBroker>,
        rabbit_publisher: Option<Arc<mq_service::SharedRabbitPublisher>>,
        redis_conn: SharedRedisConn,
        runtime_nats_publisher: SharedRuntimeNatsPublisher,
    ) -> Self {
        Self {
            pool,
            settings,
            ws_broker,
            rabbit_publisher,
            redis_conn,
            runtime_nats_publisher,
        }
    }
}

pub async fn start_server() -> Result<(), Box<dyn Error>> {
    use actix_web::{App, HttpServer};

    let settings = Settings::new()?;
    // Required for `tracing::` output in this process (same idea as `chat_worker::runner`).
    tracing_subscriber::fmt::init();
    let port = settings
        .port
        .parse::<u16>()
        .expect("Failed to parse port number");

    let pool = establish_pool(
        settings.pg_database_url.clone(),
        settings.database_pool_size,
        settings.database_connection_timeout_secs,
    );
    let ws_broker = Arc::new(ws::WsBroker::new());
    ws::run_redis_subscriber(settings.redis_url.clone(), ws_broker.clone());
    let rabbit_publisher = match mq_service::create_shared_publisher(
        &settings.rabbit_url,
        message_queue::CHAT_SESSION_EXCHANGE,
    )
    .await
    {
        Ok(p) => {
            log::info!("[rabbit] Shared publisher connected");
            Some(p)
        }
        Err(e) => {
            log::warn!(
                "[rabbit] Failed to create shared publisher (will open a new connection per publish): {}",
                e
            );
            None
        }
    };
    let redis_conn: SharedRedisConn = match redis::Client::open(settings.redis_url.as_str()) {
        Ok(client) => match client.get_multiplexed_async_connection().await {
            Ok(conn) => {
                log::info!("[redis] Publish connection connected");
                Some(Arc::new(Mutex::new(conn)))
            }
            Err(e) => {
                log::warn!("[redis] Publish connection failed: {}", e);
                None
            }
        },
        Err(e) => {
            log::warn!("[redis] Client open failed: {}", e);
            None
        }
    };
    let runtime_nats_publisher: SharedRuntimeNatsPublisher =
        match settings.runtime_nats_url.as_ref() {
            Some(url) => match RuntimeNatsPublisher::connect(url).await {
                Ok(p) => {
                    log::info!(
                        "[runtime_nats] Connected (release: stream={}, consumer={}, subject={}; env: stream={}, consumer={}, subject={})",
                        settings.runtime_nats_jetstream_stream_release,
                        settings.runtime_nats_jetstream_consumer_release,
                        settings.runtime_nats_subject_release,
                        settings.runtime_nats_jetstream_stream_env,
                        settings.runtime_nats_jetstream_consumer_env,
                        settings.runtime_nats_subject_env,
                    );
                    Some(Arc::new(p))
                }
                Err(e) => {
                    log::warn!("[runtime_nats] Failed to connect: {}", e);
                    None
                }
            },
            None => None,
        };
    let app_context = AppContext::new(
        pool,
        settings.clone(),
        ws_broker,
        rabbit_publisher,
        redis_conn,
        runtime_nats_publisher,
    );

    HttpServer::new(move || {
        App::new()
            .wrap(cors_middleware(&app_context.settings))
            .app_data(web::Data::new(app_context.clone()))
            .service(
                web::scope("/api/v1/test")
                    .wrap(from_fn(test_closed_stage_guard_middleware))
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
                web::scope("/api/v1/ws")
                    .wrap(from_fn(auth_closed_guard_middleware))
                    .route("", web::get().to(ws::handle_ws)),
            )
            .service(
                web::scope("/api/v1/platform")
                    .route(
                        "/packages-type-definition-files",
                        web::post()
                            .to(platform::handle_packages_type_definition_files)
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/package-type-definition-file-contents",
                        web::post()
                            .to(platform::handle_package_type_definition_file_contents)
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
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
                        "/projects/{id}/runtime-env-vars",
                        web::get()
                            .to(runtime_env_vars::handle_list_project_runtime_env_vars)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/runtime-env-vars",
                        web::put()
                            .to(runtime_env_vars::handle_upsert_project_runtime_env_vars)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/runtime-env-vars",
                        web::delete()
                            .to(runtime_env_vars::handle_delete_project_runtime_env_vars)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/runtime-env-vars/{key}/runtimes/{runtime_id}/reveal",
                        web::get()
                            .to(runtime_env_vars::handle_reveal_project_runtime_env_var)
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
                        "/projects/{id}/runtimes/{runtime_id}/detach-live-release",
                        web::post()
                            .to(project::handle_detach_runtime_live_release)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/runtimes/{runtime_id}/deploy-stage-release",
                        web::post()
                            .to(project::handle_deploy_stage_release)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/runtimes/{runtime_id}/deploy-production-release",
                        web::post()
                            .to(project::handle_deploy_production_release)
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
                        "/projects/{id}/releases/live-release-by-revision",
                        web::get()
                            .to(project::handle_get_project_release_by_revision)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/traces/by-ids",
                        web::post()
                            .to(project::handle_load_project_traces_by_ids)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/traces/query",
                        web::post()
                            .to(observability::handle_query_jaeger_traces)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/releases/{release_id}/units",
                        web::get()
                            .to(project::handle_list_project_release_units)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/releases/{release_id}/units/all",
                        web::post()
                            .to(project::handle_list_project_release_units_all)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/releases/{release_id}/unit-previous-by-full-ids",
                        web::post()
                            .to(project::handle_release_unit_previous_by_full_ids)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/releases/{release_id}/contexts",
                        web::get()
                            .to(project::handle_list_project_release_contexts)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/releases/{release_id}/modules-by-contexts",
                        web::post()
                            .to(project::handle_list_project_release_modules_by_contexts)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/non-draft-units",
                        web::get()
                            .to(project::handle_list_project_non_draft_units)
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
                        "/projects/{id}/chat-sessions/{session_id}/draft-units/as-of-release-controller-message/{message_id}",
                        web::get()
                            .to(chat::handle_list_chat_session_draft_units_as_of_release_controller_message)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/release-controller-messages/last-not-disabled",
                        web::get()
                            .to(chat::handle_get_last_release_controller_not_disabled)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/release-controller-messages/{message_id}/stage-release",
                        web::post()
                            .to(chat::handle_create_stage_release)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/release-controller-messages/{message_id}/production-release",
                        web::post()
                            .to(chat::handle_create_production_release)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/reject-controller-messages/{message_id}/reject",
                        web::post()
                            .to(chat::handle_confirm_reject_controller)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/user-messages/{message_id}/truncate-below",
                        web::post()
                            .to(chat::handle_truncate_chat_session_from_user_message)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/unit-tips-by-ids",
                        web::post()
                            .to(chat::handle_unit_tips_by_ids_for_chat_session)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/units-by-ids",
                        web::post()
                            .to(chat::handle_units_by_ids_for_chat_session)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/unit-latest-revisions-by-ids",
                        web::post()
                            .to(chat::handle_unit_latest_revisions_by_ids_for_chat_session)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/unit-pre-chat-base-by-paths",
                        web::post()
                            .to(chat::handle_unit_pre_chat_base_by_paths_for_chat_session)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/unit-previous-by-full-ids",
                        web::post()
                            .to(chat::handle_unit_previous_revisions_by_full_ids_for_chat_session)
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
                        "/projects/{id}/chat-sessions/{session_id}/messages/{message_id}/retry-planner",
                        web::post()
                            .to(chat::handle_retry_failed_planner_message)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    )
                    .route(
                        "/projects/{id}/chat-sessions/{session_id}/stop-generating",
                        web::post()
                            .to(chat::handle_stop_generating)
                            .wrap(from_fn(projects_owner_guard_middleware_factory()))
                            .wrap(from_fn(auth_closed_guard_middleware)),
                    ), // .route(
                       //     "/projects/{id}/chat-sessions/{session_id}/channel",
                       //     web::get()
                       //         .to(chat::handle_chat_session_channel_ws)
                       //         .wrap(from_fn(projects_owner_guard_middleware_factory()))
                       //         .wrap(from_fn(auth_closed_guard_middleware)),
                       // ),
            )
    })
    .bind((settings.bind_host.as_str(), port))?
    .run()
    .await?;

    Ok(())
}
