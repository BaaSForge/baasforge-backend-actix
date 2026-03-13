use crate::database;
use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::AccessPayload;
use crate::models::chat_messages::{
    ChatMessageCreateBody, ChatMessageInsert, ChatMessageResponse,
    ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatMessageWithFirstResponse,
    ChatSessionRoleType,
};
use crate::models::chat_sessions::{
    ChatSessionInsert, ChatSessionResponse, ChatSessionWithProjectRuntimeResponse,
};
use crate::models::projects::PlatformEnvironmentType;
use crate::models::projects::{ProjectRuntimeResponse, ProjectUnitResponse};
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::routes::AppContext;
// use crate::services::message_queue as mq_service;
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::project_runtimes;
use crate::schema::project_units;
use crate::schema::projects;
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
// use actix_ws::Message;
use chrono::Utc;
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::json;
// use tokio_stream::StreamExt as _;

pub async fn handle_list_project_chat_sessions(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project chat sessions");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    let sessions = match database::block_on_db(&context.pool, move |conn| {
        chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .order(chat_sessions::created_at.desc())
            .select(ChatSessionResponse::as_select())
            .load::<ChatSessionResponse>(conn)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            tracing::error!("[handle_list_project_chat_sessions] DB/block error");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Project chat sessions fetched successfully",
        json!(sessions),
    ))
}

pub async fn handle_get_project_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(i64, i64)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get project chat session");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let response = match database::block_on_db(&context.pool, move |conn| {
        let session = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .filter(chat_sessions::id.eq(session_id))
            .select(ChatSessionResponse::as_select())
            .get_result::<ChatSessionResponse>(conn)?;

        let project_runtime = session.stage_project_runtime_id.and_then(|runtime_id| {
            project_runtimes::table
                .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
                .filter(projects::owner_user_id.eq(user_id))
                .filter(project_runtimes::project_id.eq(project_id))
                .filter(project_runtimes::id.eq(runtime_id))
                .select(ProjectRuntimeResponse::as_select())
                .get_result::<ProjectRuntimeResponse>(conn)
                .ok()
        });

        Ok::<_, diesel::result::Error>(ChatSessionWithProjectRuntimeResponse {
            session,
            project_runtime,
        })
    })
    .await
    {
        Ok(r) => r,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_get_project_chat_session] DB/block error");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Chat session fetched successfully",
        json!(response),
    ))
}

const DEFAULT_MESSAGES_PAGE_SIZE: i64 = 50;
const MAX_MESSAGES_PAGE_SIZE: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct ListChatMessagesQuery {
    pub before_id: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn handle_list_chat_session_messages(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(i64, i64)>,
    query: web::Query<ListChatMessagesQuery>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list chat session messages");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let limit = query
        .limit
        .map(|l| l.clamp(1, MAX_MESSAGES_PAGE_SIZE))
        .unwrap_or(DEFAULT_MESSAGES_PAGE_SIZE);
    let before_id = query.before_id;

    let messages_with_first = match database::block_on_db(&context.pool, move |conn| {
        let mut db_query = chat_messages::table
            .inner_join(chat_sessions::table.on(chat_messages::chat_session_id.eq(chat_sessions::id)))
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .filter(chat_messages::chat_session_id.eq(session_id))
            .into_boxed();

        if let Some(bid) = before_id {
            db_query = db_query.filter(chat_messages::id.lt(bid));
        }

        let rows = db_query
            .select(ChatMessageRow::as_select())
            .order(chat_messages::id.desc())
            .limit(limit)
            .load::<ChatMessageRow>(conn)
            .unwrap_or_default();
        let messages: Vec<ChatMessageResponse> = rows.into_iter().map(ChatMessageResponse::from).collect();

        let oldest_id: Option<i64> = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .select(diesel::dsl::min(chat_messages::id))
            .get_result(conn)
            .ok()
            .flatten();

        Ok::<_, diesel::result::Error>(messages
            .into_iter()
            .map(|m| ChatMessageWithFirstResponse {
                is_first: oldest_id.map_or(false, |oldest| m.id == oldest),
                inner: m,
            })
            .collect::<Vec<_>>())
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            tracing::error!("[handle_list_chat_session_messages] DB/block error");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Chat session messages fetched successfully",
        json!(messages_with_first),
    ))
}

pub async fn handle_create_chat_session_message(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(i64, i64)>,
    body: web::Json<ChatMessageCreateBody>,
) -> HttpResponse {
    let user_id = req
        .extensions()
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to create chat session message")
        .user_id;
    let (project_id, session_id) = path.into_inner();
    let body = body.into_inner();

    if body.type_ != ChatMessageType::Text {
        return HttpResponse::BadRequest().json(api_error(
            "Only text type messages are allowed for this endpoint",
            json!({}),
        ));
    }

    if let Err(e) = body.validate_payload() {
        return HttpResponse::BadRequest().json(api_error(
            "Invalid payload for message type",
            json!({ "detail": e.to_string() }),
        ));
    }

    let insert = ChatMessageInsert {
        chat_session_id: session_id,
        role: ChatSessionRoleType::User,
        type_: body.type_.clone(),
        payload: body.payload.clone(),
        status: ChatMessageStatus::Completed,
    };

    let (message, is_first) = match database::block_on_db(&context.pool, move |conn| {
        let session_check: Option<i64> = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first::<i64>(conn)
            .optional()?;
        if session_check.is_none() {
            return Err(diesel::result::Error::NotFound);
        }
        let row: ChatMessageRow = diesel::insert_into(chat_messages::table)
            .values(&insert)
            .returning(ChatMessageRow::as_select())
            .get_result(conn)?;
        let message = ChatMessageResponse::from(row);
        let count: i64 = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .count()
            .get_result(conn)?;
        Ok((message, count == 1))
    })
    .await
    {
        Ok((msg, first)) => (msg, first),
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            tracing::error!("DB block error on create chat message");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Service is busy. Please try again shortly.",
                json!({}),
            ));
        }
    };

    let queue_message = ChatSessionQueueMessage {
        user_id: Some(user_id),
        role: ChatSessionRoleType::User,
        type_: message.type_.clone(),
        payload: message.payload.clone(),
    };
    let publish_result = if let Some(ref publisher) = context.rabbit_publisher {
        chat_message_queue::publish_with_publisher(publisher, project_id, session_id, &queue_message).await
    } else {
        tracing::error!("No RabbitMQ publisher available on create chat message");
        return HttpResponse::ServiceUnavailable().json(api_error(
            "Service is busy. Please try again shortly.",
            json!({}),
        ));
    };
    if let Err(e) = publish_result {
        tracing::error!(
            %project_id,
            %session_id,
            "Failed to publish chat message to queue: {}",
            e
        );
        return HttpResponse::ServiceUnavailable().json(api_error(
            "Message was saved but the assistant pipeline is temporarily unavailable. Please try again.",
            json!({ "detail": e.to_string() }),
        ));
    }

    let response = ChatMessageWithFirstResponse {
        inner: message,
        is_first,
    };

    HttpResponse::Ok().json(api_success(
        "Chat message created successfully",
        json!(response),
    ))
}

pub async fn handle_list_chat_session_draft_units(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(i64, i64)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list chat session draft units");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let units = match database::block_on_db(&context.pool, move |conn| {
        let session_exists = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first::<i64>(conn)
            .optional()?;
        if session_exists.is_none() {
            return Err(diesel::result::Error::NotFound);
        }
        project_units::table
            .filter(project_units::chat_session_id.eq(session_id))
            .filter(project_units::is_draft.eq(true))
            .select(ProjectUnitResponse::as_select())
            .load::<ProjectUnitResponse>(conn)
    })
    .await
    {
        Ok(v) => v,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_list_chat_session_draft_units] DB/block error");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Chat session draft units fetched successfully",
        json!(units),
    ))
}

pub async fn handle_create_project_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to create project chat session");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    let chat_session = match database::block_on_db(&context.pool, move |conn| {
        let chat_session = ChatSessionInsert {
            project_id,
            user_id,
            title: format!("New chat session {}", Utc::now().format("%H:%M")),
            stage_project_runtime_id: None,
        };
        diesel::insert_into(chat_sessions::table)
            .values(&chat_session)
            .returning(ChatSessionResponse::as_select())
            .get_result::<ChatSessionResponse>(conn)
    })
    .await
    {
        Ok(s) => s,
        Err(_) => {
            tracing::error!("[handle_create_project_chat_session] DB/block error");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Project chat session created successfully",
        json!(chat_session),
    ))
}

#[derive(Debug, Deserialize)]
pub struct AttachStageRuntimeBody {
    pub project_runtime_id: i64,
}

pub async fn handle_attach_chat_session_stage_runtime(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(i64, i64)>,
    body: web::Json<AttachStageRuntimeBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to set chat session stage runtime");

    let (project_id, session_id) = path.into_inner();
    let project_runtime_id = body.project_runtime_id;
    let user_id = access_payload.user_id;

    enum AttachOut {
        RuntimeNotFound,
        SessionNotFound,
        AlreadyAttached,
        Ok(ChatSessionResponse),
    }
    let updated = match database::block_on_db(&context.pool, move |conn| {
        let _runtime = project_runtimes::table
            .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_runtimes::project_id.eq(project_id))
            .filter(project_runtimes::id.eq(project_runtime_id))
            .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Stage))
            .select(project_runtimes::id)
            .get_result::<i64>(conn)
            .optional()?;
        if _runtime.is_none() {
            return Ok(AttachOut::RuntimeNotFound);
        }
        let session_row: Option<(i64, Option<i64>)> = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select((chat_sessions::id, chat_sessions::stage_project_runtime_id))
            .first(conn)
            .optional()?;
        let (_, current_runtime_id) = match session_row {
            Some(row) => row,
            None => return Ok(AttachOut::SessionNotFound),
        };
        if current_runtime_id.is_some() {
            return Ok(AttachOut::AlreadyAttached);
        }
        let u = diesel::update(
            chat_sessions::table
                .filter(chat_sessions::id.eq(session_id))
                .filter(chat_sessions::project_id.eq(project_id)),
        )
        .set(chat_sessions::stage_project_runtime_id.eq(project_runtime_id))
        .returning(ChatSessionResponse::as_select())
        .get_result::<ChatSessionResponse>(conn)?;
        Ok(AttachOut::Ok(u))
    })
    .await
    {
        Ok(AttachOut::Ok(u)) => u,
        Ok(AttachOut::RuntimeNotFound) => {
            return HttpResponse::BadRequest().json(api_error(
                "Project runtime not found or is not a stage environment",
                json!({}),
            ));
        }
        Ok(AttachOut::SessionNotFound) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Ok(AttachOut::AlreadyAttached) => {
            return HttpResponse::Conflict().json(api_error(
                "Chat session already has a stage runtime attached. Detach the current runtime first.",
                json!({}),
            ));
        }
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_attach_chat_session_stage_runtime] DB/block error");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    // TODO: RabbitMQ – Publish message when stage runtime is set.

    HttpResponse::Ok().json(api_success(
        "Chat session stage runtime updated successfully",
        json!(updated),
    ))
}

#[derive(Debug, Deserialize)]
pub struct DetachStageRuntimeBody {
    pub project_runtime_id: i64,
}

pub async fn handle_detach_chat_session_stage_runtime(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(i64, i64)>,
    body: web::Json<DetachStageRuntimeBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to detach chat session stage runtime");

    let (project_id, session_id) = path.into_inner();
    let project_runtime_id = body.project_runtime_id;
    let user_id = access_payload.user_id;

    enum DetachOut {
        SessionNotFound,
        NoRuntime,
        WrongRuntime,
        Ok(ChatSessionResponse),
    }
    let updated = match database::block_on_db(&context.pool, move |conn| {
        let session_row: Option<(i64, Option<i64>)> = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select((chat_sessions::id, chat_sessions::stage_project_runtime_id))
            .first(conn)
            .optional()?;
        let (_, current_runtime_id) = match session_row {
            Some(row) => row,
            None => return Ok(DetachOut::SessionNotFound),
        };
        let current_runtime_id = match current_runtime_id {
            Some(id) => id,
            None => return Ok(DetachOut::NoRuntime),
        };
        if current_runtime_id != project_runtime_id {
            return Ok(DetachOut::WrongRuntime);
        }
        let u = diesel::update(
            chat_sessions::table
                .filter(chat_sessions::id.eq(session_id))
                .filter(chat_sessions::project_id.eq(project_id)),
        )
        .set(chat_sessions::stage_project_runtime_id.eq(Option::<i64>::None))
        .returning(ChatSessionResponse::as_select())
        .get_result::<ChatSessionResponse>(conn)?;
        Ok(DetachOut::Ok(u))
    })
    .await
    {
        Ok(DetachOut::Ok(u)) => u,
        Ok(DetachOut::SessionNotFound) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Ok(DetachOut::NoRuntime) => {
            return HttpResponse::BadRequest().json(api_error(
                "Chat session has no stage runtime attached",
                json!({}),
            ));
        }
        Ok(DetachOut::WrongRuntime) => {
            return HttpResponse::BadRequest().json(api_error(
                "Chat session is not attached to the given runtime",
                json!({}),
            ));
        }
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_detach_chat_session_stage_runtime] DB/block error");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Database temporarily unavailable",
                json!({}),
            ));
        }
    };

    // TODO: prune target runtime deployment (server & db)

    HttpResponse::Ok().json(api_success(
        "Chat session stage runtime detached successfully",
        json!(updated),
    ))
}