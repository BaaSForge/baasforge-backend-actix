use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::AccessPayload;
use crate::models::chat_messages::{
    ChatMessageCreateBody, ChatMessageInsert, ChatMessageResponse, ChatMessageType,
    ChatSessionRoleType,
};
use crate::models::chat_sessions::{ChatSessionInsert, ChatSessionResponse};
use crate::models::projects::ProjectUnitResponse;
use crate::routes::AppContext;
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::project_units;
use crate::schema::projects;
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use chrono::Utc;
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

pub async fn handle_create_chat_session_message(
    context: web::Data<AppContext>,
    path: web::Path<(i64, i64)>,
    body: web::Json<ChatMessageCreateBody>,
) -> HttpResponse {
    let (project_id, session_id) = path.into_inner();
    let body = body.into_inner();

    if body.type_ != ChatMessageType::Text {
        return HttpResponse::BadRequest().json(api_error(
            "Only text type messages are allowed for this endpoint",
            json!({}),
        ));
    }

    let mut conn = context.pool.get().expect("DB conn");

    match chat_sessions::table
        .filter(chat_sessions::id.eq(session_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .select(chat_sessions::id)
        .get_result::<i64>(&mut conn)
    {
        Ok(_) => {}
        Err(diesel::result::Error::NotFound) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(e) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
    }

    let insert = ChatMessageInsert {
        chat_session_id: session_id,
        role: ChatSessionRoleType::User,
        type_: body.type_,
        payload: body.payload,
    };

    let message: ChatMessageResponse = diesel::insert_into(chat_messages::table)
        .values(&insert)
        .returning(ChatMessageResponse::as_select())
        .get_result(&mut conn)
        .expect("Insert chat message failed");

    HttpResponse::Ok().json(api_success(
        "Chat message created successfully",
        json!(message),
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

    let mut conn = context.pool.get().expect("DB conn");

    // Verify the user has access to this chat session
    let session_exists = chat_sessions::table
        .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(chat_sessions::id.eq(session_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .select(chat_sessions::id)
        .first::<i64>(&mut conn)
        .optional()
        .expect("DB query failed");

    if session_exists.is_none() {
        return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
    }

    let units = project_units::table
        .filter(project_units::chat_session_id.eq(session_id))
        .filter(project_units::is_draft.eq(true))
        .select(ProjectUnitResponse::as_select())
        .load::<ProjectUnitResponse>(&mut conn)
        .unwrap_or_default();

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
