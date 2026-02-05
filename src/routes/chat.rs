use crate::libs::api_response::api_success;
use crate::middlewares::auth::AccessPayload;
use crate::models::chat_sessions::ChatSessionResponse;
use crate::routes::AppContext;
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
