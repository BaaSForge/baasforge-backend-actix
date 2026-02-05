use crate::libs::api_response::api_success;
use crate::middlewares::auth::AccessPayload;
use crate::models::chat_messages::ChatMessageResponse;
use crate::models::chat_sessions::{ChatSessionInsert, ChatSessionResponse};
use crate::routes::AppContext;
use chrono::Utc;
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::projects;
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::prelude::*;
use serde_json::json;

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

    let mut conn = context.pool.get().expect("DB conn");

    let sessions = chat_sessions::table
        .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .select(ChatSessionResponse::as_select())
        .load::<ChatSessionResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Project chat sessions fetched successfully",
        json!(sessions),
    ))
}

pub async fn handle_list_chat_session_messages(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(i64, i64)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list chat session messages");

    let (project_id, session_id) = path.into_inner();

    let mut conn = context.pool.get().expect("DB conn");

    let messages = chat_messages::table
        .inner_join(chat_sessions::table.on(chat_messages::chat_session_id.eq(chat_sessions::id)))
        .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .filter(chat_messages::chat_session_id.eq(session_id))
        .select(ChatMessageResponse::as_select())
        .order(chat_messages::created_at.asc())
        .load::<ChatMessageResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Chat session messages fetched successfully",
        json!(messages),
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

    let mut conn = context.pool.get().expect("DB conn");

    let chat_session = ChatSessionInsert {
        project_id: project_id,
        user_id: access_payload.user_id,
        title: format!("New chat session {}", Utc::now().format("%H:%M")),
    };

    let chat_session: ChatSessionResponse = diesel::insert_into(chat_sessions::table)
        .values(&chat_session)
        .returning(ChatSessionResponse::as_select())
        .get_result(&mut conn)
        .expect("Insert failed");

    HttpResponse::Ok().json(api_success(
        "Project chat session created successfully",
        json!(chat_session),
    ))
}
