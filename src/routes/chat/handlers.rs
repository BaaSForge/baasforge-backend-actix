use crate::database;
use crate::libs::api_response::{api_error, api_success};
use crate::libs::baal_inline::expand_baal_inlines_for_chat_title;
use crate::libs::ids::new_uuid;
use crate::libs::string::truncate_by_words;
use crate::libs::unit_slug::{
    UnitPath, normalize_full_unit_id_arg, parse_unit_path, validate_full_unit_id_for_tool,
    validate_full_unit_id_revision_segment,
};
use crate::middlewares::auth::AccessPayload;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageCreateBody, ChatMessageInsert, ChatMessagePayload,
    ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
    ChatMessageWithFirstResponse, ChatSessionRoleType, TextPayload,
};
use crate::models::chat_sessions::{
    ChatSessionDetailResponse, ChatSessionInsert, ChatSessionResponse,
    ChatSessionWithLastMessageResponse,
};
use crate::models::projects::{
    PlatformEnvironmentType, ProjectReleaseResponse, ProjectReleaseWithBaseline,
    ProjectUnitResponse,
};
use crate::routes::{
    AppContext, ensure_runtime_nats_available, runtime_nats_required_publisher,
    runtime_nats_unavailable_response,
};
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::routes::ws::events::{
    ChatSessionGenerationEndedPayload, ChatSessionGenerationStartedPayload,
    ChatSessionMessageStatusChangePayload, ChatSessionMessageUpdatedPayload,
    ProductionLiveReleaseChangedPayload, WsServerEvent,
};
use crate::routes::ws::redis::{publish_event_to_channel, user_channel};
use crate::routes::ws::set_stopped_message_id;
// use crate::services::message_queue as mq_service;
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::project_releases;
use crate::schema::project_runtimes;
use crate::schema::project_units;
use crate::schema::projects;
use crate::services::chat_session_truncate::{
    RetryFailedPlannerTruncateOutcome, TruncateNatsContext, TruncateSessionFromMessageOutcome,
    TruncateWithNatsError, transaction_retry_failed_planner_truncate_and_reset_with_nats,
    truncate_session_from_user_message_with_nats,
};
use crate::services::runtime_nats::RuntimeNatsPublishError;
use crate::services::chat_worker::db::{
    ChatSessionListReleasePair, ChatSessionListUnitFilter, DraftUnitsAsOfReleaseControllerError,
    LastReleaseControllerNotDisabledLookup, list_project_chat_sessions_page,
    load_chat_session_draft_units_as_of_release_controller_message,
    load_chat_session_unit_tips_for_merge, load_last_not_disabled_release_controller_for_session,
    load_latest_revisions_by_unit_paths, load_owned_releases_by_chat_session,
    load_owned_unit_tips_for_chat_sessions, load_pre_chat_base_units_by_unit_paths,
    load_unit_previous_revisions_by_full_ids_for_chat_session,
    load_unit_tips_by_ids_for_chat_session, load_units_by_ids_for_chat_session,
};
use crate::services::chat_worker::reject_controller_box::apply_disabled_due_to_live_production_release_on_matching_reject_box;
use crate::services::releases::{
    all_live_production_release_ids,
    assistant_turn_has_live_production_release_with_live_ids, attach_base_production_releases,
    attach_is_draft_flags_from_latest_production, enrich_chat_message_response_releases,
    enrich_owned_releases_by_session_map, insert_production_release_from_stage_row,
    live_release_id_from_production_runtime, prod_live_release_source_chat_session,
    production_release_id_for_live_runtimes,
    recompute_user_message_edit_disabled_for_session_live_production,
    stale_base_release_from_baseline,
};
use crate::services::stage_release_from_controller::{
    StageReleaseFromControllerError, transaction_create_stage_release_from_controller,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
// use actix_ws::Message;
use chrono::Utc;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;
// use tokio_stream::StreamExt as _;

fn default_chat_sessions_list_page() -> u32 {
    1
}

fn default_chat_sessions_list_page_size() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
pub struct ListProjectChatSessionsQuery {
    #[serde(default = "default_chat_sessions_list_page")]
    pub page: u32,
    #[serde(default = "default_chat_sessions_list_page_size")]
    pub page_size: u32,
    #[serde(default)]
    pub release_filters: Option<String>,
    #[serde(default)]
    pub unit_filters: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatSessionReleaseFilterJson {
    env: PlatformEnvironmentType,
    revision: serde_json::Value,
}

fn parse_release_filters_json(
    raw: &Option<String>,
) -> Result<Vec<ChatSessionListReleasePair>, String> {
    let Some(s) = raw.as_ref().map(|x| x.trim()).filter(|x| !x.is_empty()) else {
        return Ok(Vec::new());
    };
    let items: Vec<ChatSessionReleaseFilterJson> =
        serde_json::from_str(s).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let revision = match &it.revision {
            serde_json::Value::Number(n) => n
                .as_f64()
                .or_else(|| n.as_i64().map(|i| i as f64))
                .ok_or_else(|| "release_filters: invalid revision number".to_string())?,
            serde_json::Value::String(st) => st
                .trim()
                .parse::<f64>()
                .map_err(|_| "release_filters: invalid revision string".to_string())?,
            _ => {
                return Err("release_filters: revision must be a JSON number or string".to_string());
            }
        };
        if !revision.is_finite() {
            return Err("release_filters: revision must be finite".to_string());
        }
        out.push(ChatSessionListReleasePair {
            env: it.env,
            revision,
        });
    }
    Ok(out)
}

fn parse_unit_filters_json(raw: &Option<String>) -> Result<Vec<ChatSessionListUnitFilter>, String> {
    let Some(s) = raw.as_ref().map(|x| x.trim()).filter(|x| !x.is_empty()) else {
        return Ok(Vec::new());
    };
    let items: Vec<String> = serde_json::from_str(s).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(parse_chat_session_list_unit_token(&item)?);
    }
    Ok(out)
}

fn parse_chat_session_list_unit_token(item: &str) -> Result<ChatSessionListUnitFilter, String> {
    let s = normalize_full_unit_id_arg(item);
    let parts: Vec<&str> = s.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() == 5 && parts[4].chars().all(|c| c.is_ascii_digit()) {
        validate_full_unit_id_revision_segment(&s)?;
        return Ok(ChatSessionListUnitFilter::FullId(s));
    }
    parse_unit_path(&s)
        .map(ChatSessionListUnitFilter::Path)
}

pub async fn handle_list_project_chat_sessions(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    query: web::Query<ListProjectChatSessionsQuery>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project chat sessions");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;
    let q = query.into_inner();

    let release_pairs = match parse_release_filters_json(&q.release_filters) {
        Ok(v) => v,
        Err(msg) => {
            return HttpResponse::BadRequest().json(api_error(&msg, json!({})));
        }
    };
    let unit_filters = match parse_unit_filters_json(&q.unit_filters) {
        Ok(v) => v,
        Err(msg) => {
            return HttpResponse::BadRequest().json(api_error(&msg, json!({})));
        }
    };

    let page_req = q.page;
    let page_size_req = q.page_size;

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        let (ids, total, page, page_size) = list_project_chat_sessions_page(
            conn,
            user_id,
            project_id,
            &release_pairs,
            &unit_filters,
            page_req,
            page_size_req,
        )?;

        let (current_live, live_source_session) =
            prod_live_release_source_chat_session(conn, project_id)?;

        if ids.is_empty() {
            return Ok::<_, diesel::result::Error>((Vec::new(), total, page, page_size));
        }

        let sessions: Vec<ChatSessionResponse> = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .filter(chat_sessions::id.eq_any(&ids))
            .select(ChatSessionResponse::as_select())
            .load::<ChatSessionResponse>(conn)?;

        let mut by_id: HashMap<Uuid, ChatSessionResponse> =
            sessions.into_iter().map(|s| (s.id, s)).collect();
        let sessions_ordered: Vec<ChatSessionResponse> =
            ids.iter().filter_map(|id| by_id.remove(id)).collect();

        let session_ids: Vec<Uuid> = sessions_ordered.iter().map(|s| s.id).collect();
        let latest_message_rows = chat_messages::table
            .inner_join(
                chat_sessions::table.on(chat_messages::chat_session_id.eq(chat_sessions::id)),
            )
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .filter(chat_messages::chat_session_id.eq_any(&session_ids))
            .filter(chat_messages::type_.eq(ChatMessageType::Text))
            .distinct_on(chat_messages::chat_session_id)
            .order((
                chat_messages::chat_session_id.asc(),
                chat_messages::created_at.desc(),
                chat_messages::id.desc(),
            ))
            .select(ChatMessageRow::as_select())
            .load::<ChatMessageRow>(conn)?;

        let mut latest_by_session_id: HashMap<Uuid, ChatMessageResponse> = HashMap::new();
        for row in latest_message_rows {
            let message = ChatMessageResponse::from(row);
            latest_by_session_id.insert(message.chat_session_id, message);
        }

        let owned_releases_by_session =
            load_owned_releases_by_chat_session(conn, project_id, &session_ids)?;
        let owned_releases_by_session =
            enrich_owned_releases_by_session_map(conn, project_id, owned_releases_by_session)?;
        let owned_units_by_session =
            load_owned_unit_tips_for_chat_sessions(conn, project_id, &session_ids)?;

        let sessions_with_last_message: Vec<ChatSessionWithLastMessageResponse> = sessions_ordered
            .into_iter()
            .map(|session| {
                let stale_base_release = stale_base_release_from_baseline(
                    session.live_production_release_id,
                    current_live,
                    live_source_session,
                    session.id,
                );
                ChatSessionWithLastMessageResponse {
                    stale_base_release,
                    last_message: latest_by_session_id.remove(&session.id),
                    owned_units: owned_units_by_session
                        .get(&session.id)
                        .cloned()
                        .unwrap_or_default(),
                    owned_releases: owned_releases_by_session
                        .get(&session.id)
                        .cloned()
                        .unwrap_or_default(),
                    session,
                }
            })
            .collect();

        Ok::<_, diesel::result::Error>((sessions_with_last_message, total, page, page_size))
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            tracing::error!("[handle_list_project_chat_sessions] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let (sessions_with_last_message, total, page, page_size) = outcome;

    HttpResponse::Ok().json(api_success(
        "Project chat sessions fetched successfully",
        json!({
            "sessions": sessions_with_last_message,
            "total": total,
            "page": page,
            "page_size": page_size,
        }),
    ))
}

pub async fn handle_get_project_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
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

        let (current_live, live_source_session) =
            prod_live_release_source_chat_session(conn, project_id)?;
        let stale_base_release = stale_base_release_from_baseline(
            session.live_production_release_id,
            current_live,
            live_source_session,
            session.id,
        );

        Ok::<_, diesel::result::Error>(ChatSessionDetailResponse {
            session,
            stale_base_release,
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
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
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
    pub before_id: Option<Uuid>,
    pub limit: Option<i64>,
}

pub async fn handle_list_chat_session_messages(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
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
            .inner_join(
                chat_sessions::table.on(chat_messages::chat_session_id.eq(chat_sessions::id)),
            )
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
            .load::<ChatMessageRow>(conn)?;
        let messages: Vec<ChatMessageResponse> = rows
            .into_iter()
            .map(ChatMessageResponse::from)
            .map(|m| enrich_chat_message_response_releases(conn, project_id, m))
            .collect::<Result<Vec<_>, _>>()?;

        // UUID doesn't support min() in Diesel; get oldest by created_at
        let oldest_id: Option<Uuid> = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .order(chat_messages::created_at.asc())
            .select(chat_messages::id)
            .first(conn)
            .optional()?;

        Ok::<_, diesel::result::Error>(
            messages
                .into_iter()
                .map(|m| ChatMessageWithFirstResponse {
                    is_first: oldest_id == Some(m.id),
                    inner: m,
                })
                .collect::<Vec<_>>(),
        )
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            tracing::error!("[handle_list_chat_session_messages] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Chat session messages fetched successfully",
        json!(messages_with_first),
    ))
}

enum GetLastReleaseControllerNotDisabledOutcome {
    SessionNotFound,
    NoEligibleMessage,
    Message(ChatMessageResponse),
}

fn strip_units_metadata_from_release_controller_response(
    mut msg: ChatMessageResponse,
) -> ChatMessageResponse {
    let Some(ChatMessagePayload::ReleaseControllerBox(mut p)) = msg.payload.take() else {
        return msg;
    };
    if let Some(s) = p.stage_release.as_mut() {
        s.release.units_metadata = None;
        if let Some(b) = s.base_production_release.as_mut() {
            b.units_metadata = None;
        }
    }
    if let Some(pr) = p.prod_release.as_mut() {
        pr.release.units_metadata = None;
        if let Some(b) = pr.base_production_release.as_mut() {
            b.units_metadata = None;
        }
    }
    msg.payload = Some(ChatMessagePayload::ReleaseControllerBox(p));
    msg
}

pub async fn handle_get_last_release_controller_not_disabled(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get last release controller message");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        match load_last_not_disabled_release_controller_for_session(
            conn, user_id, project_id, session_id,
        )? {
            LastReleaseControllerNotDisabledLookup::SessionNotFound => {
                Ok(GetLastReleaseControllerNotDisabledOutcome::SessionNotFound)
            }
            LastReleaseControllerNotDisabledLookup::NoEligibleMessage => {
                Ok(GetLastReleaseControllerNotDisabledOutcome::NoEligibleMessage)
            }
            LastReleaseControllerNotDisabledLookup::Found(row) => {
                let msg = enrich_chat_message_response_releases(
                    conn,
                    project_id,
                    ChatMessageResponse::from(row),
                )?;
                Ok(GetLastReleaseControllerNotDisabledOutcome::Message(
                    strip_units_metadata_from_release_controller_response(msg),
                ))
            }
        }
    })
    .await
    {
        Ok(o) => o,
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_get_last_release_controller_not_disabled] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        GetLastReleaseControllerNotDisabledOutcome::SessionNotFound => {
            HttpResponse::NotFound().json(api_error("Chat session not found", json!({})))
        }
        GetLastReleaseControllerNotDisabledOutcome::NoEligibleMessage => {
            HttpResponse::Ok().json(api_success(
                "No release controller message eligible in this session",
                json!({ "message": serde_json::Value::Null }),
            ))
        }
        GetLastReleaseControllerNotDisabledOutcome::Message(m) => {
            HttpResponse::Ok().json(api_success(
                "Last eligible release controller message",
                json!({ "message": m }),
            ))
        }
    }
}

pub async fn handle_create_chat_session_message(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
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

    // Reject if there is already a message being generated in this chat session
    match database::block_on_db(&context.pool, {
        let session_id = session_id;
        move |conn| {
            let session_check: Option<Uuid> = chat_sessions::table
                .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
                .filter(projects::owner_user_id.eq(user_id))
                .filter(chat_sessions::id.eq(session_id))
                .filter(chat_sessions::project_id.eq(project_id))
                .select(chat_sessions::id)
                .first::<Uuid>(conn)
                .optional()?;
            if session_check.is_none() {
                return Err(diesel::result::Error::NotFound);
            }
            let generating_count: i64 = chat_messages::table
                .filter(chat_messages::chat_session_id.eq(session_id))
                .filter(chat_messages::role.eq(ChatSessionRoleType::Ai))
                .filter(chat_messages::status.eq(ChatMessageStatus::Generating))
                .count()
                .get_result(conn)?;
            Ok(generating_count > 0)
        }
    })
    .await
    {
        Ok(true) => {
            return HttpResponse::Conflict().json(api_error(
                "A message is already being generated in this chat. Please wait for it to finish.",
                json!({}),
            ));
        }
        Ok(false) => {}
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            tracing::error!("DB block error on create chat message (generating check)");
            return HttpResponse::ServiceUnavailable().json(api_error(
                "Service is busy. Please try again shortly.",
                json!({}),
            ));
        }
    };

    let user_insert = ChatMessageInsert {
        chat_session_id: session_id,
        role: ChatSessionRoleType::User,
        type_: body.type_,
        payload: body.payload.clone(),
        status: ChatMessageStatus::Completed,
        agent: None, // user message; agent is only set for AI role
        is_edit_disabled_due_to_live_production_release: false,
    };

    let (message, is_first) = match database::block_on_db(&context.pool, move |conn| {
        let session_check: Option<Uuid> = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first::<Uuid>(conn)
            .optional()?;
        if session_check.is_none() {
            return Err(diesel::result::Error::NotFound);
        }
        let row: ChatMessageRow = diesel::insert_into(chat_messages::table)
            .values(&user_insert)
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

    let response_message_id = new_uuid();
    let queue_payload: Option<serde_json::Value> = match &message.payload {
        Some(p) => match serde_json::to_value(p) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::error!("serialize chat message for queue: {}", e);
                return HttpResponse::InternalServerError().json(api_error(
                    "Failed to serialize message for processing queue",
                    json!({}),
                ));
            }
        },
        None => None,
    };
    let queue_message = ChatSessionQueueMessage {
        user_id,
        role: ChatSessionRoleType::Ai,
        type_: message.type_,
        payload: queue_payload,
        response_message_id,
        retry_num: 0,
        invoking_message_id: None,
        agent: ChatAgentType::Planner,
        sub_agent_stuck_continuation_count: 0,
        assistant_turn_id: None,
    };
    let rabbit_timeout =
        std::time::Duration::from_secs(context.settings.rabbit_operation_timeout_secs);
    let publish_result = if let Some(ref publisher) = context.rabbit_publisher {
        chat_message_queue::publish_with_publisher_timed(
            publisher,
            project_id,
            session_id,
            &queue_message,
            rabbit_timeout,
        )
        .await
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

    if let Some(ref redis_conn) = context.redis_conn {
        let topic = user_channel(user_id);
        let payload = ChatSessionGenerationStartedPayload {
            project_id,
            session_id,
        };
        let event = WsServerEvent::ChatSessionGenerationStarted(payload);
        let mut guard = crate::services::redis_conn::clone_connection(redis_conn.clone());
        if let Err(e) = publish_event_to_channel(&mut guard, &topic, &event).await {
            tracing::warn!("Failed to broadcast generation started to Redis: {}", e);
        }
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

/// Re-queues the worker for a **main-thread Planner** AI `Text` row that ended in `Failed` (e.g. LLM
/// provider error after Rabbit retries). Uses the same empty-queue payload as an automatic post-tool
/// continuation: planner history is loaded from completed session rows only, so the user line and
/// prior planner turns remain in context without duplicating the last user text.
pub async fn handle_retry_failed_planner_message(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid, Uuid)>,
) -> HttpResponse {
    let user_id = req
        .extensions()
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to retry planner message")
        .user_id;
    let (project_id, session_id, message_id) = path.into_inner();

    if let Err(e) = ensure_runtime_nats_available(context.get_ref()).await {
        return runtime_nats_unavailable_response(&e);
    }

    let nats_ctx = TruncateNatsContext {
        nats_required: context.settings.runtime_nats_url.is_some(),
        publisher: context.runtime_nats_publisher.as_deref(),
        subject: context.settings.runtime_nats_subject_release.as_str(),
        project_id,
    };

    let outcome = match transaction_retry_failed_planner_truncate_and_reset_with_nats(
        &context.pool,
        nats_ctx,
        user_id,
        project_id,
        session_id,
        message_id,
    )
    .await
    {
        Ok(o) => o,
        Err(TruncateWithNatsError::Nats(e)) => {
            return runtime_nats_unavailable_response(&e);
        }
        Err(TruncateWithNatsError::Db(database::BlockingDbError::Query(e))) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(TruncateWithNatsError::Db(_)) => {
            tracing::error!("[handle_retry_failed_planner_message] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        RetryFailedPlannerTruncateOutcome::SessionNotFound => {
            HttpResponse::NotFound().json(api_error("Chat session not found", json!({})))
        }
        RetryFailedPlannerTruncateOutcome::AlreadyGenerating => HttpResponse::Conflict().json(api_error(
            "A message is already being generated in this chat. Please wait for it to finish.",
            json!({}),
        )),
        RetryFailedPlannerTruncateOutcome::MessageNotFound => {
            HttpResponse::NotFound().json(api_error("Chat message not found in this session", json!({})))
        }
        RetryFailedPlannerTruncateOutcome::NotEligible { reason } => HttpResponse::UnprocessableEntity().json(
            api_error(
                "This message cannot be retried as a failed planner turn",
                json!({ "reason": reason }),
            ),
        ),
        RetryFailedPlannerTruncateOutcome::HasLiveProductionReleaseAfterMessage => {
            HttpResponse::BadRequest().json(api_error(
                "Cannot retry: a live production release exists on a release controller after this message",
                json!({}),
            ))
        }
        RetryFailedPlannerTruncateOutcome::Ok => {
            let continuation_payload = match serde_json::to_value(ChatMessagePayload::Text(
                TextPayload {
                    text: String::new(),
                    ..Default::default()
                },
            )) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("retry planner: serialize continuation payload: {}", e);
                    let _ = database::block_on_db(&context.pool, move |conn| {
                        diesel::update(
                            chat_messages::table
                                .filter(chat_messages::id.eq(message_id))
                                .filter(chat_messages::chat_session_id.eq(session_id)),
                        )
                        .set(chat_messages::status.eq(ChatMessageStatus::Failed))
                        .execute(conn)
                    })
                    .await;
                    return HttpResponse::InternalServerError().json(api_error(
                        "Failed to prepare retry payload",
                        json!({}),
                    ));
                }
            };
            let queue_message = ChatSessionQueueMessage {
                user_id,
                role: ChatSessionRoleType::Ai,
                type_: ChatMessageType::Text,
                payload: Some(continuation_payload),
                response_message_id: message_id,
                retry_num: 0,
                invoking_message_id: None,
                agent: ChatAgentType::Planner,
                sub_agent_stuck_continuation_count: 0,
                assistant_turn_id: None,
            };
            let Some(ref publisher) = context.rabbit_publisher else {
                tracing::error!("No RabbitMQ publisher available on retry planner message");
                let _ = database::block_on_db(&context.pool, move |conn| {
                    diesel::update(
                        chat_messages::table
                            .filter(chat_messages::id.eq(message_id))
                            .filter(chat_messages::chat_session_id.eq(session_id)),
                    )
                    .set(chat_messages::status.eq(ChatMessageStatus::Failed))
                    .execute(conn)
                })
                .await;
                return HttpResponse::ServiceUnavailable().json(api_error(
                    "Service is busy. Please try again shortly.",
                    json!({}),
                ));
            };
            let rabbit_timeout = std::time::Duration::from_secs(
                context.settings.rabbit_operation_timeout_secs,
            );
            if let Err(e) = chat_message_queue::publish_with_publisher_timed(
                publisher,
                project_id,
                session_id,
                &queue_message,
                rabbit_timeout,
            )
            .await
            {
                tracing::error!(
                    %project_id,
                    %session_id,
                    %message_id,
                    "Failed to publish planner retry to queue: {}",
                    e
                );
                let revert = database::block_on_db(&context.pool, move |conn| {
                    diesel::update(
                        chat_messages::table
                            .filter(chat_messages::id.eq(message_id))
                            .filter(chat_messages::chat_session_id.eq(session_id)),
                    )
                    .set(chat_messages::status.eq(ChatMessageStatus::Failed))
                    .execute(conn)
                })
                .await;
                if let Err(rev_e) = revert {
                    tracing::error!(
                        %message_id,
                        "Failed to revert message to Failed after queue publish error: {}",
                        rev_e
                    );
                }
                return HttpResponse::ServiceUnavailable().json(api_error(
                    "Planner retry was prepared but the assistant pipeline is temporarily unavailable. Please try again.",
                    json!({ "detail": e.to_string() }),
                ));
            }

            if let Some(ref redis_conn) = context.redis_conn {
                let topic = user_channel(user_id);
                let mut guard = crate::services::redis_conn::clone_connection(redis_conn.clone());
                let status_event =
                    WsServerEvent::ChatSessionMessageStatusChange(ChatSessionMessageStatusChangePayload {
                        project_id,
                        session_id,
                        message_id,
                        status: ChatMessageStatus::Generating,
                    });
                if let Err(e) = publish_event_to_channel(&mut guard, &topic, &status_event).await {
                    tracing::warn!(
                        "retry planner: failed to publish status Generating to Redis: {}",
                        e
                    );
                }
                let gen_event = WsServerEvent::ChatSessionGenerationStarted(
                    ChatSessionGenerationStartedPayload {
                        project_id,
                        session_id,
                    },
                );
                if let Err(e) = publish_event_to_channel(&mut guard, &topic, &gen_event).await {
                    tracing::warn!(
                        "retry planner: failed to broadcast generation started to Redis: {}",
                        e
                    );
                }
            }

            HttpResponse::Ok().json(api_success(
                "Planner retry queued",
                json!({
                    "message_id": message_id,
                    "session_id": session_id,
                    "project_id": project_id,
                }),
            ))
        }
    }
}

pub async fn handle_stop_generating(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to stop generating");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let ttl_secs = context.settings.stopped_messages_ids_cache_ttl_secs;
    let session_id_for_update = session_id;
    let project_id_ws = project_id;
    let session_id_ws = session_id;

    enum StopOut {
        NotFound,
        Ok(Vec<Uuid>),
    }

    let generating_message_ids = match database::block_on_db(&context.pool, move |conn| {
        let session_exists: Option<Uuid> = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first(conn)
            .optional()?;
        if session_exists.is_none() {
            return Ok(StopOut::NotFound);
        }
        let ids: Vec<Uuid> = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(chat_messages::status.eq(ChatMessageStatus::Generating))
            .select(chat_messages::id)
            .load::<Uuid>(conn)?;
        Ok(StopOut::Ok(ids))
    })
    .await
    {
        Ok(StopOut::NotFound) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Ok(StopOut::Ok(ids)) => ids,
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_stop_generating] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    if generating_message_ids.is_empty() {
        return HttpResponse::Ok().json(api_success(
            "No generating messages to stop",
            json!({ "stopped_count": 0, "stopped_message_ids": [] }),
        ));
    }

    let update_result = database::block_on_db(&context.pool, move |conn| {
        diesel::update(
            chat_messages::table
                .filter(chat_messages::chat_session_id.eq(session_id_for_update))
                .filter(chat_messages::status.eq(ChatMessageStatus::Generating)),
        )
        .set(chat_messages::status.eq(ChatMessageStatus::Stopped))
        .execute(conn)
    })
    .await;

    match update_result {
        Ok(_) => {}
        Err(database::BlockingDbError::Query(e)) => {
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_stop_generating] DB/block error on update");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    }

    if let Some(ref redis_conn) = context.redis_conn {
        let mut guard = crate::services::redis_conn::clone_connection(redis_conn.clone());
        for message_id in &generating_message_ids {
            if let Err(e) = set_stopped_message_id(&mut guard, *message_id, ttl_secs).await {
                tracing::warn!(
                    %message_id,
                    "Failed to set stopped message id in Redis: {}",
                    e
                );
            }
        }
        let channel = user_channel(user_id);
        for message_id in &generating_message_ids {
            let event = WsServerEvent::ChatSessionMessageStatusChange(
                ChatSessionMessageStatusChangePayload {
                    project_id: project_id_ws,
                    session_id: session_id_ws,
                    message_id: *message_id,
                    status: ChatMessageStatus::Stopped,
                },
            );
            if let Err(e) = publish_event_to_channel(&mut guard, &channel, &event).await {
                tracing::warn!(
                    %message_id,
                    "Failed to publish message status change to Redis: {}",
                    e
                );
            }
        }
        let event = WsServerEvent::ChatSessionGenerationEnded(ChatSessionGenerationEndedPayload {
            project_id: project_id_ws,
            session_id: session_id_ws,
        });
        if let Err(e) = publish_event_to_channel(&mut guard, &channel, &event).await {
            tracing::warn!("Failed to publish generation ended to Redis: {}", e);
        }
    }

    HttpResponse::Ok().json(api_success(
        "Generating stopped",
        json!({
            "stopped_count": generating_message_ids.len(),
            "stopped_message_ids": generating_message_ids,
        }),
    ))
}

pub async fn handle_list_chat_session_draft_units(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
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
            .first::<Uuid>(conn)
            .optional()?;
        if session_exists.is_none() {
            return Err(diesel::result::Error::NotFound);
        }
        load_chat_session_unit_tips_for_merge(conn, project_id, session_id).and_then(|mut units| {
            units.sort_by(|a, b| {
                a.context_slug
                    .cmp(&b.context_slug)
                    .then_with(|| a.module_slug.cmp(&b.module_slug))
                    .then_with(|| a.unit_name_slug.cmp(&b.unit_name_slug))
            });
            attach_is_draft_flags_from_latest_production(conn, project_id, units)
        })
    })
    .await
    {
        Ok(v) => v,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_list_chat_session_draft_units] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Chat session draft units fetched successfully",
        json!(units),
    ))
}

pub async fn handle_list_chat_session_draft_units_as_of_release_controller_message(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid, Uuid)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions.get::<AccessPayload>().expect(
        "AccessPayload must exist to list chat session draft units as of release controller",
    );

    let (project_id, session_id, release_controller_message_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let units = match database::block_on_db(&context.pool, move |conn| {
        let session_exists = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first::<Uuid>(conn)
            .optional()?;
        if session_exists.is_none() {
            return Err(diesel::result::Error::NotFound);
        }
        match load_chat_session_draft_units_as_of_release_controller_message(
            conn,
            project_id,
            session_id,
            release_controller_message_id,
        )? {
            Ok(units) => {
                let units = attach_is_draft_flags_from_latest_production(conn, project_id, units)?;
                Ok(Ok(units))
            }
            Err(e) => Ok(Err(e)),
        }
    })
    .await
    {
        Ok(Ok(units)) => units,
        Ok(Err(DraftUnitsAsOfReleaseControllerError::MessageNotFound)) => {
            return HttpResponse::NotFound().json(api_error(
                "Release controller message not found for this chat session",
                json!({}),
            ));
        }
        Ok(Err(DraftUnitsAsOfReleaseControllerError::NotReleaseControllerMessage)) => {
            return HttpResponse::BadRequest().json(api_error(
                "Message is not a release_controller_box row",
                json!({}),
            ));
        }
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(_) => {
            tracing::error!(
                "[handle_list_chat_session_draft_units_as_of_release_controller_message] DB/block error"
            );
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Chat session draft units fetched successfully",
        json!(units),
    ))
}

const MAX_UNIT_TIPS_BY_IDS: usize = 100;

#[derive(Debug, Deserialize)]
pub struct UnitTipsByIdsBody {
    pub unit_ids: Vec<String>,
}

/// Body for endpoints that accept dot-separated **unit paths** (four segments) or full ids (five
/// segments including revision), not bare stored ids alone.
#[derive(Debug, Deserialize)]
pub struct UnitPathsBody {
    pub unit_paths: Vec<String>,
}

#[derive(Serialize)]
struct UnitTipsByIdsResponse {
    /// Same length and order as request `unit_ids`; `null` when the id is unknown in this session.
    units: Vec<Option<ProjectUnitResponse>>,
}

/// Resolves the current revision **tip** (and `latest_revision`) per stored unit id in this session.
pub async fn handle_unit_tips_by_ids_for_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<UnitTipsByIdsBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to resolve chat session unit tips");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let unit_ids = &body.unit_ids;

    if unit_ids.len() > MAX_UNIT_TIPS_BY_IDS {
        return HttpResponse::BadRequest().json(api_error(
            "Too many unit ids in one request",
            json!({ "max": MAX_UNIT_TIPS_BY_IDS }),
        ));
    }

    let response = match database::block_on_db(&context.pool, {
        let unit_ids = unit_ids.clone();
        move |conn| {
            let session_exists = chat_sessions::table
                .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
                .filter(projects::owner_user_id.eq(user_id))
                .filter(chat_sessions::id.eq(session_id))
                .filter(chat_sessions::project_id.eq(project_id))
                .select(chat_sessions::id)
                .first::<Uuid>(conn)
                .optional()?;
            if session_exists.is_none() {
                return Err(diesel::result::Error::NotFound);
            }
            let units =
                load_unit_tips_by_ids_for_chat_session(conn, project_id, session_id, &unit_ids)?;
            Ok::<_, diesel::result::Error>(UnitTipsByIdsResponse { units })
        }
    })
    .await
    {
        Ok(v) => v,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_unit_tips_by_ids_for_chat_session] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Chat session unit tips resolved successfully",
        json!(response),
    ))
}

#[derive(Serialize)]
struct UnitsByIdsResponse {
    /// Same length and order as request `unit_ids`; `null` when the id is unknown in this session.
    units: Vec<Option<ProjectUnitResponse>>,
}

/// Returns the stored unit row at each id (specific revision), with `latest_revision` set to the
/// max revision for that logical unit. Does not replace the row with the current tip.
pub async fn handle_units_by_ids_for_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<UnitTipsByIdsBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to resolve chat session units by ids");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let unit_ids = &body.unit_ids;

    if unit_ids.len() > MAX_UNIT_TIPS_BY_IDS {
        return HttpResponse::BadRequest().json(api_error(
            "Too many unit ids in one request",
            json!({ "max": MAX_UNIT_TIPS_BY_IDS }),
        ));
    }

    let response = match database::block_on_db(&context.pool, {
        let unit_ids = unit_ids.clone();
        move |conn| {
            let session_exists = chat_sessions::table
                .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
                .filter(projects::owner_user_id.eq(user_id))
                .filter(chat_sessions::id.eq(session_id))
                .filter(chat_sessions::project_id.eq(project_id))
                .select(chat_sessions::id)
                .first::<Uuid>(conn)
                .optional()?;
            if session_exists.is_none() {
                return Err(diesel::result::Error::NotFound);
            }
            let units =
                load_units_by_ids_for_chat_session(conn, project_id, session_id, &unit_ids)?;
            Ok::<_, diesel::result::Error>(UnitsByIdsResponse { units })
        }
    })
    .await
    {
        Ok(v) => v,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(_) => {
            tracing::error!("[handle_units_by_ids_for_chat_session] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Chat session units by ids resolved successfully",
        json!(response),
    ))
}

#[derive(Serialize)]
struct UnitLatestRevisionsResponse {
    /// Same length and order as request `unit_paths`; `null` when no unit exists for that path in this session.
    revisions: Vec<Option<i32>>,
}

/// For each **unit path** (four segments: `context.module.unit_type.unit_name`) or a **full unit
/// id** (five segments; revision segment ignored), returns the largest `revision` in this chat session.
pub async fn handle_unit_latest_revisions_by_ids_for_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<UnitPathsBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to resolve unit latest revisions");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let paths = &body.unit_paths;

    if paths.len() > MAX_UNIT_TIPS_BY_IDS {
        return HttpResponse::BadRequest().json(api_error(
            "Too many unit paths in one request",
            json!({ "max": MAX_UNIT_TIPS_BY_IDS }),
        ));
    }

    let mut unit_paths: Vec<UnitPath> = Vec::with_capacity(paths.len());
    for path_str in paths {
        match parse_unit_path(path_str) {
            Ok(p) => unit_paths.push(p),
            Err(msg) => {
                return HttpResponse::BadRequest().json(api_error(
                    "Invalid unit path",
                    json!({ "unit_path": path_str, "detail": msg }),
                ));
            }
        }
    }

    let response = match database::block_on_db(&context.pool, move |conn| {
        let session_exists = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first::<Uuid>(conn)
            .optional()?;
        if session_exists.is_none() {
            return Err(diesel::result::Error::NotFound);
        }
        let revisions =
            load_latest_revisions_by_unit_paths(conn, project_id, session_id, &unit_paths)?;
        Ok(UnitLatestRevisionsResponse { revisions })
    })
    .await
    {
        Ok(v) => v,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(database::BlockingDbError::Query(_)) => {
            tracing::error!(
                "[handle_unit_latest_revisions_by_ids_for_chat_session] DB/block error"
            );
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
        Err(_) => {
            tracing::error!(
                "[handle_unit_latest_revisions_by_ids_for_chat_session] pool/blocking error"
            );
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Latest unit revisions resolved successfully",
        json!(response),
    ))
}

#[derive(Serialize)]
struct UnitPreChatBaseByPathsResponse {
    /// Same length and order as request `unit_paths`; `null` when the session has no baseline
    /// production release, the baseline snapshot has no row for that path, or the baseline id is stale.
    units: Vec<Option<ProjectUnitResponse>>,
}

/// For each unit path, returns the unit from this session’s baseline production release combined
/// snapshot (same logical unit), with `latest_revision` for the owning session.
pub async fn handle_unit_pre_chat_base_by_paths_for_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<UnitPathsBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to resolve pre-chat base units");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let paths = &body.unit_paths;

    if paths.len() > MAX_UNIT_TIPS_BY_IDS {
        return HttpResponse::BadRequest().json(api_error(
            "Too many unit paths in one request",
            json!({ "max": MAX_UNIT_TIPS_BY_IDS }),
        ));
    }

    let mut unit_paths: Vec<UnitPath> = Vec::with_capacity(paths.len());
    for path_str in paths {
        match parse_unit_path(path_str) {
            Ok(p) => unit_paths.push(p),
            Err(msg) => {
                return HttpResponse::BadRequest().json(api_error(
                    "Invalid unit path",
                    json!({ "unit_path": path_str, "detail": msg }),
                ));
            }
        }
    }

    let response = match database::block_on_db(&context.pool, move |conn| {
        let session_exists = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first::<Uuid>(conn)
            .optional()?;
        if session_exists.is_none() {
            return Err(diesel::result::Error::NotFound);
        }
        let units =
            load_pre_chat_base_units_by_unit_paths(conn, project_id, session_id, &unit_paths)?;
        Ok(UnitPreChatBaseByPathsResponse { units })
    })
    .await
    {
        Ok(v) => v,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
        }
        Err(database::BlockingDbError::Query(_)) => {
            tracing::error!("[handle_unit_pre_chat_base_by_paths_for_chat_session] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
        Err(_) => {
            tracing::error!(
                "[handle_unit_pre_chat_base_by_paths_for_chat_session] pool/blocking error"
            );
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Pre-chat base unit rows resolved successfully",
        json!(response),
    ))
}

#[derive(Serialize)]
struct UnitPreviousByFullIdsResponse {
    units: Vec<Option<ProjectUnitResponse>>,
}

pub async fn handle_unit_previous_revisions_by_full_ids_for_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<UnitTipsByIdsBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to resolve unit previous revisions");

    let (project_id, session_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let unit_ids = &body.unit_ids;

    if unit_ids.len() > MAX_UNIT_TIPS_BY_IDS {
        return HttpResponse::BadRequest().json(api_error(
            "Too many unit ids in one request",
            json!({ "max": MAX_UNIT_TIPS_BY_IDS }),
        ));
    }

    for id in unit_ids {
        if let Err(msg) = validate_full_unit_id_for_tool(id) {
            return HttpResponse::BadRequest().json(api_error(
                "Invalid full unit id",
                json!({ "unit_id": id, "detail": msg }),
            ));
        }
    }

    let session_ok = match database::block_on_db(&context.pool, move |conn| {
        chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first::<Uuid>(conn)
            .optional()
            .map(|o| o.is_some())
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            tracing::error!(
                "[handle_unit_previous_revisions_by_full_ids_for_chat_session] session check pool/block error"
            );
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    if !session_ok {
        return HttpResponse::NotFound().json(api_error("Chat session not found", json!({})));
    }

    let response = match database::block_on_db(&context.pool, {
        let unit_ids = unit_ids.clone();
        move |conn| {
            let (units, missing) = load_unit_previous_revisions_by_full_ids_for_chat_session(
                conn, project_id, session_id, &unit_ids,
            )?;
            if let Some(id) = missing {
                return Ok(Err(id));
            }
            Ok(Ok(UnitPreviousByFullIdsResponse { units }))
        }
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(unit_id)) => {
            return HttpResponse::NotFound().json(api_error(
                "Previous unit revision not found in this chat session or its baseline production release",
                json!({ "unit_id": unit_id }),
            ));
        }
        Err(database::BlockingDbError::Query(_)) => {
            tracing::error!(
                "[handle_unit_previous_revisions_by_full_ids_for_chat_session] DB/block error"
            );
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
        Err(_) => {
            tracing::error!(
                "[handle_unit_previous_revisions_by_full_ids_for_chat_session] pool/block error"
            );
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Previous unit revisions resolved successfully",
        json!(response),
    ))
}

/// Must stay in sync with `chat_sessions.title` (`VARCHAR(150)` in migrations / `schema.rs`).
const CHAT_SESSION_TITLE_MAX_CHARS: usize = 120;

fn chat_session_title_from_string(preferredString: Option<String>) -> String {
    let fallback = || format!("New chat session {}", Utc::now().format("%H:%M"));

    preferredString
        .map(|m| expand_baal_inlines_for_chat_title(&m))
        .and_then(|m| truncate_by_words(m, 15))
        .map(|s| s.chars().take(CHAT_SESSION_TITLE_MAX_CHARS).collect())
        .unwrap_or_else(fallback)
}

pub async fn handle_create_project_chat_session(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    body: web::Json<CreateProjectChatSessionBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to create project chat session");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;
    let title = chat_session_title_from_string(body.into_inner().title);

    let chat_session = match database::block_on_db(&context.pool, move |conn| {
        let live_production_release_id = live_release_id_from_production_runtime(conn, project_id)?;
        let chat_session = ChatSessionInsert {
            project_id,
            user_id,
            title,
            live_production_release_id,
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
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Project chat session created successfully",
        json!(ChatSessionDetailResponse {
            session: chat_session,
            stale_base_release: false,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectChatSessionBody {
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateStageReleaseBody {
    pub stage_project_runtime_id: Uuid,
}

/// Body for POST `.../production-release`. `stage_release_id` must match the stage release on the
/// release controller message for `{message_id}`.
#[derive(Debug, Deserialize)]
pub struct CreateProductionReleaseBody {
    pub stage_release_id: Uuid,
}

enum ProdReleaseOutcome {
    Success(
        ProjectReleaseResponse,
        ChatMessageRow,
        Option<Uuid>,
        Option<ChatMessageRow>,
        Vec<ChatMessageRow>,
    ),
    SessionNotFound,
    MessageNotFound,
    NotReleaseController,
    ReleaseControllerPayloadMissing,
    ReleaseControllerPayloadInvalid,
    NoStageRelease,
    StageReleaseIdMismatch,
    StageReleaseNotFoundForSession,
    ProdRuntimeNotFound,
    Metadata(String),
}

pub fn http_response_for_stage_release_from_controller_error(
    e: StageReleaseFromControllerError,
) -> HttpResponse {
    use StageReleaseFromControllerError::*;
    match e {
        SessionNotFound => HttpResponse::NotFound().json(api_error("Chat session not found", json!({}))),
        MessageNotFound => HttpResponse::NotFound()
            .json(api_error("Release controller message not found", json!({}))),
        NotReleaseController => HttpResponse::BadRequest().json(api_error(
            "Message is not a release_controller_box row",
            json!({}),
        )),
        ReleaseControllerPayloadMissing => HttpResponse::BadRequest().json(api_error(
            "Release controller message has no payload yet; wait for the session to finish updating or refresh",
            json!({}),
        )),
        ReleaseControllerPayloadInvalid => HttpResponse::BadRequest().json(api_error(
            "Release controller message payload is missing or invalid; try refreshing the chat",
            json!({}),
        )),
        RuntimeNotFound => HttpResponse::NotFound().json(api_error(
            "Stage project runtime not found for this project",
            json!({}),
        )),
        RuntimeNotStage => HttpResponse::BadRequest().json(api_error(
            "Referenced runtime is not a stage environment",
            json!({}),
        )),
        Metadata(msg) => HttpResponse::InternalServerError()
            .json(api_error(&format!("Failed to build release metadata: {}", msg), json!({}))),
    }
}

/// After production’s live release changes, notify the project owner so the client can refetch runtimes.
pub async fn broadcast_production_live_release_changed(
    context: &web::Data<AppContext>,
    project_id: Uuid,
) {
    let Some(ref redis_conn) = context.redis_conn else {
        return;
    };
    let pool = context.pool.clone();
    let owner_user_id = match database::block_on_db(&pool, move |conn| {
        projects::table
            .filter(projects::id.eq(project_id))
            .select(projects::owner_user_id)
            .first::<Uuid>(conn)
            .optional()
    })
    .await
    {
        Ok(Some(id)) => id,
        Ok(None) => {
            tracing::warn!(
                project_id = %project_id,
                "[broadcast_production_live_release_changed] project not found"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(?e, "[broadcast_production_live_release_changed] DB failed");
            return;
        }
    };

    let event = WsServerEvent::ProductionLiveReleaseChanged(ProductionLiveReleaseChangedPayload {
        project_id,
    });
    let mut guard = crate::services::redis_conn::clone_connection(redis_conn.clone());
    if let Err(e) =
        publish_event_to_channel(&mut guard, &user_channel(owner_user_id), &event).await
    {
        tracing::warn!(
            "[broadcast_production_live_release_changed] WS publish failed: {}",
            e
        );
    }
}

/// WS + NATS after a stage release was created from a release-controller message (shared with detach-live-release).
pub async fn broadcast_stage_release_create_effects(
    context: &web::Data<AppContext>,
    project_id: Uuid,
    session_id: Uuid,
    user_id: Uuid,
    stage_runtime_id: Uuid,
    release: &ProjectReleaseResponse,
    row: ChatMessageRow,
    previous_stage_release_id: Option<Uuid>,
) -> Result<(), RuntimeNatsPublishError> {
    if let Some(ref redis_conn) = context.redis_conn {
        let message: ChatMessageResponse = row.into();
        let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
            project_id,
            session_id,
            message,
        });
        let mut guard = crate::services::redis_conn::clone_connection(redis_conn.clone());
        if let Err(e) = publish_event_to_channel(&mut guard, &user_channel(user_id), &event).await
        {
            tracing::warn!(
                "[broadcast_stage_release_create_effects] WS publish failed: {}",
                e
            );
        }
    }
    if context.settings.runtime_nats_url.is_some() {
        let nats = runtime_nats_required_publisher(context.get_ref())?;
        let subject = context.settings.runtime_nats_subject_release.as_str();
        nats.publish_stage_release(
            subject,
            project_id,
            stage_runtime_id,
            release.id,
            previous_stage_release_id,
        )
        .await?;
    }
    Ok(())
}

/// POST — create a stage release from the current session draft unit tips vs latest production,
/// attach it to this chat session, set that revision on the given stage runtime, and refresh the
/// release controller box payload + WS update.
pub async fn handle_create_stage_release(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid, Uuid)>,
    body: web::Json<CreateStageReleaseBody>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let (project_id, session_id, message_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let stage_runtime_id = body.stage_project_runtime_id;

    if let Err(e) = ensure_runtime_nats_available(context.get_ref()).await {
        return runtime_nats_unavailable_response(&e);
    }

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        transaction_create_stage_release_from_controller(
            conn,
            user_id,
            project_id,
            session_id,
            message_id,
            stage_runtime_id,
        )
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("[handle_create_stage_release] DB error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        Ok((release, row, previous_stage_release_id)) => {
            let release_for_broadcast = release.clone();
            let release_with_base = match database::block_on_db(&context.pool, move |conn| {
                attach_base_production_releases(conn, project_id, vec![release])
            })
            .await
            {
                Ok(mut v) => v.pop().expect("one release"),
                Err(_) => {
                    tracing::error!(
                        "[handle_create_stage_release] DB/block error (attach baseline)"
                    );
                    return HttpResponse::ServiceUnavailable()
                        .json(api_error("Database temporarily unavailable", json!({})));
                }
            };
            if let Err(e) = broadcast_stage_release_create_effects(
                &context,
                project_id,
                session_id,
                user_id,
                stage_runtime_id,
                &release_for_broadcast,
                row,
                previous_stage_release_id,
            )
            .await
            {
                return runtime_nats_unavailable_response(&e);
            }
            HttpResponse::Ok().json(api_success(
                "Stage release created successfully",
                json!({ "release": release_with_base }),
            ))
        }
        Err(e) => http_response_for_stage_release_from_controller_error(e),
    }
}

/// POST — promote a stage release to production: request body carries `stage_release_id`; the server
/// loads that stage row and inserts a production `project_releases` row with the same
/// `units_metadata` (no draft/merge recomputation—those ran when the stage release was created).
pub async fn handle_create_production_release(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid, Uuid)>,
    body: web::Json<CreateProductionReleaseBody>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let (project_id, session_id, message_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let stage_release_id = body.stage_release_id;

    if let Err(e) = ensure_runtime_nats_available(context.get_ref()).await {
        return runtime_nats_unavailable_response(&e);
    }

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        conn.transaction(|conn| {
            let session_ok = chat_sessions::table
                .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
                .filter(projects::owner_user_id.eq(user_id))
                .filter(chat_sessions::id.eq(session_id))
                .filter(chat_sessions::project_id.eq(project_id))
                .select(chat_sessions::id)
                .first::<Uuid>(conn)
                .optional()?;
            if session_ok.is_none() {
                return Ok(ProdReleaseOutcome::SessionNotFound);
            }

            let msg_row: Option<ChatMessageRow> = chat_messages::table
                .filter(chat_messages::id.eq(message_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)
                .optional()?;
            let Some(msg_row) = msg_row else {
                return Ok(ProdReleaseOutcome::MessageNotFound);
            };
            if msg_row.type_ != ChatMessageType::ReleaseControllerBox {
                return Ok(ProdReleaseOutcome::NotReleaseController);
            }

            let mut box_payload =
                match ChatMessagePayload::from_value(&msg_row.type_, msg_row.payload.clone()) {
                    Ok(Some(ChatMessagePayload::ReleaseControllerBox(p))) => p,
                    Ok(None) => return Ok(ProdReleaseOutcome::ReleaseControllerPayloadMissing),
                    Ok(Some(other)) => {
                        tracing::warn!(
                            message_id = %message_id,
                            variant = other.variant_label(),
                            "release_controller_box row has unexpected payload variant"
                        );
                        return Ok(ProdReleaseOutcome::ReleaseControllerPayloadInvalid);
                    }
                    Err(e) => {
                        tracing::warn!(
                            message_id = %message_id,
                            error = %e,
                            "release_controller_box payload deserialize failed"
                        );
                        return Ok(ProdReleaseOutcome::ReleaseControllerPayloadInvalid);
                    }
                };

            let Some(stage_on_controller) = box_payload.stage_release.as_ref() else {
                return Ok(ProdReleaseOutcome::NoStageRelease);
            };
            if stage_release_id != stage_on_controller.release.id {
                return Ok(ProdReleaseOutcome::StageReleaseIdMismatch);
            }
            if let Some(cid) = stage_on_controller.release.chat_session_id
                && cid != session_id
            {
                return Ok(ProdReleaseOutcome::StageReleaseNotFoundForSession);
            }

            // Stage must be a row for this project, this session, and stage lane (not another chat).
            let Some(stage) = project_releases::table
                .filter(project_releases::id.eq(stage_release_id))
                .filter(project_releases::project_id.eq(project_id))
                .filter(project_releases::chat_session_id.eq(Some(session_id)))
                .filter(project_releases::is_stage.eq(true))
                .select(ProjectReleaseResponse::as_select())
                .first(conn)
                .optional()?
            else {
                return Ok(ProdReleaseOutcome::StageReleaseNotFoundForSession);
            };

            let previous_prod_release_id =
                production_release_id_for_live_runtimes(conn, project_id)?;

            let inserted = insert_production_release_from_stage_row(
                conn, project_id, session_id, message_id, &stage,
            )?;

            let n = diesel::update(
                project_runtimes::table
                    .filter(project_runtimes::project_id.eq(project_id))
                    .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Prod)),
            )
            .set(project_runtimes::live_release_id.eq(Some(inserted.id)))
            .execute(conn)?;
            if n == 0 {
                return Ok(ProdReleaseOutcome::ProdRuntimeNotFound);
            }

            let base_prod: Option<ProjectReleaseResponse> =
                match inserted.base_production_release_id {
                    Some(bid) => project_releases::table
                        .filter(project_releases::id.eq(bid))
                        .filter(project_releases::project_id.eq(project_id))
                        .select(ProjectReleaseResponse::as_select())
                        .first(conn)
                        .optional()?,
                    None => None,
                };
            box_payload.prod_release = Some(ProjectReleaseWithBaseline {
                release: inserted.clone(),
                base_production_release: base_prod,
            });
            let payload_json = match serde_json::to_value(&box_payload) {
                Ok(v) => v,
                Err(e) => return Ok(ProdReleaseOutcome::Metadata(e.to_string())),
            };

            let updated_row =
                diesel::update(chat_messages::table.filter(chat_messages::id.eq(message_id)))
                    .set(chat_messages::payload.eq(Some(payload_json)))
                    .returning(ChatMessageRow::as_returning())
                    .get_result(conn)?;

            let reject_updated_row = match box_payload.assistant_turn_id {
                Some(tid) => apply_disabled_due_to_live_production_release_on_matching_reject_box(
                    conn, session_id, tid,
                )?,
                None => None,
            };

            let user_messages_edit_disabled =
                recompute_user_message_edit_disabled_for_session_live_production(
                    conn,
                    project_id,
                    session_id,
                )?;

            Ok(ProdReleaseOutcome::Success(
                inserted,
                updated_row,
                previous_prod_release_id,
                reject_updated_row,
                user_messages_edit_disabled,
            ))
        })
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("[handle_create_production_release] DB error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        ProdReleaseOutcome::Success(
            release,
            row,
            previous_prod_release_id,
            reject_row_opt,
            user_messages_edit_disabled,
        ) => {
            let release_for_attach = release.clone();
            let release_for_response = match database::block_on_db(&context.pool, move |conn| {
                attach_base_production_releases(conn, project_id, vec![release_for_attach])
            })
            .await
            {
                Ok(mut v) => v.pop().expect("one release"),
                Err(_) => {
                    tracing::error!(
                        "[handle_create_production_release] DB/block error (attach baseline)"
                    );
                    return HttpResponse::ServiceUnavailable()
                        .json(api_error("Database temporarily unavailable", json!({})));
                }
            };
            if let Some(ref redis_conn) = context.redis_conn {
                let message: ChatMessageResponse = row.into();
                let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
                    project_id,
                    session_id,
                    message,
                });
                let mut guard = crate::services::redis_conn::clone_connection(redis_conn.clone());
                if let Err(e) = publish_event_to_channel(&mut guard, &user_channel(user_id), &event).await
                {
                    tracing::warn!("[handle_create_production_release] WS publish failed: {}", e);
                }
                if let Some(reject_row) = reject_row_opt {
                    let message_reject: ChatMessageResponse = reject_row.into();
                    let event_reject =
                        WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
                            project_id,
                            session_id,
                            message: message_reject,
                        });
                    if let Err(e) =
                        publish_event_to_channel(&mut guard, &user_channel(user_id), &event_reject)
                            .await
                    {
                        tracing::warn!(
                            "[handle_create_production_release] WS publish (reject box) failed: {}",
                            e
                        );
                    }
                }
                for user_row in user_messages_edit_disabled {
                    let message_user: ChatMessageResponse = user_row.into();
                    let event_user =
                        WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
                            project_id,
                            session_id,
                            message: message_user,
                        });
                    if let Err(e) =
                        publish_event_to_channel(&mut guard, &user_channel(user_id), &event_user)
                            .await
                    {
                        tracing::warn!(
                            "[handle_create_production_release] WS publish (user edit flag) failed: {}",
                            e
                        );
                    }
                }
            }
            broadcast_production_live_release_changed(&context, project_id).await;
            if context.settings.runtime_nats_url.is_some() {
                let nats = match runtime_nats_required_publisher(context.get_ref()) {
                    Ok(nats) => nats,
                    Err(e) => return runtime_nats_unavailable_response(&e),
                };
                let subject = context.settings.runtime_nats_subject_release.as_str();
                if let Err(e) = nats
                    .publish_prod_release(
                        subject,
                        project_id,
                        release.id,
                        previous_prod_release_id,
                    )
                    .await
                {
                    return runtime_nats_unavailable_response(&e);
                }
            }
            HttpResponse::Ok().json(api_success(
                "Production release created successfully",
                json!({ "release": release_for_response }),
            ))
        }
        ProdReleaseOutcome::SessionNotFound => HttpResponse::NotFound()
            .json(api_error("Chat session not found", json!({}))),
        ProdReleaseOutcome::MessageNotFound => HttpResponse::NotFound()
            .json(api_error("Release controller message not found", json!({}))),
        ProdReleaseOutcome::NotReleaseController => HttpResponse::BadRequest().json(
            api_error("Message is not a release_controller_box row", json!({})),
        ),
        ProdReleaseOutcome::ReleaseControllerPayloadMissing => HttpResponse::BadRequest().json(
            api_error(
                "Release controller message has no payload yet; wait for the session to finish updating or refresh",
                json!({}),
            ),
        ),
        ProdReleaseOutcome::ReleaseControllerPayloadInvalid => HttpResponse::BadRequest().json(
            api_error(
                "Release controller message payload is missing or invalid; try refreshing the chat",
                json!({}),
            ),
        ),
        ProdReleaseOutcome::NoStageRelease => HttpResponse::BadRequest().json(api_error(
            "No stage release on this controller; create a stage release first",
            json!({}),
        )),
        ProdReleaseOutcome::StageReleaseIdMismatch => HttpResponse::BadRequest().json(api_error(
            "stage_release_id does not match the stage release on this release controller message",
            json!({}),
        )),
        ProdReleaseOutcome::StageReleaseNotFoundForSession => HttpResponse::BadRequest().json(
            api_error(
                "No stage release with this id for this chat session: the stage row must belong to the session in the URL (create stage from this session first)",
                json!({}),
            ),
        ),
        ProdReleaseOutcome::ProdRuntimeNotFound => HttpResponse::BadRequest().json(api_error(
            "No production runtime exists for this project",
            json!({}),
        )),
        ProdReleaseOutcome::Metadata(msg) => HttpResponse::InternalServerError()
            .json(api_error(&format!("Failed to serialize message payload: {}", msg), json!({}))),
    }
}

#[derive(Debug)]
enum ConfirmRejectOutcome {
    Success(
        ChatMessageRow,
        Option<ChatMessageRow>,
        Option<Uuid>,
        Vec<Uuid>,
    ),
    SessionNotFound,
    MessageNotFound,
    NotRejectController,
    PayloadInvalid,
    AlreadyRejected,
    DisabledByLiveProductionRelease,
    NotLastMessage,
}

/// POST — set `is_rejected` on a `reject_controller_box` row when it is still actionable (last session
/// message, not blocked by a live production release on the paired controller), disable the paired
/// release controller, delete its stage and non-live production release rows (detaching any stage
/// runtime that had that stage release live), and clear those references from the controller payload.
pub async fn handle_confirm_reject_controller(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid, Uuid)>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let (project_id, session_id, message_id) = path.into_inner();
    let user_id = access_payload.user_id;

    if let Err(e) = ensure_runtime_nats_available(context.get_ref()).await {
        return runtime_nats_unavailable_response(&e);
    }

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        conn.transaction(|conn| -> Result<ConfirmRejectOutcome, diesel::result::Error> {
            let session_ok = chat_sessions::table
                .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
                .filter(projects::owner_user_id.eq(user_id))
                .filter(chat_sessions::id.eq(session_id))
                .filter(chat_sessions::project_id.eq(project_id))
                .select(chat_sessions::id)
                .first::<Uuid>(conn)
                .optional()?;
            if session_ok.is_none() {
                return Ok(ConfirmRejectOutcome::SessionNotFound);
            }

            let msg_row: Option<ChatMessageRow> = chat_messages::table
                .filter(chat_messages::id.eq(message_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)
                .optional()?;
            let Some(msg_row) = msg_row else {
                return Ok(ConfirmRejectOutcome::MessageNotFound);
            };
            if msg_row.type_ != ChatMessageType::RejectControllerBox {
                return Ok(ConfirmRejectOutcome::NotRejectController);
            }

            let mut box_payload = match ChatMessagePayload::from_value(
                &msg_row.type_,
                msg_row.payload.clone(),
            ) {
                Ok(Some(ChatMessagePayload::RejectControllerBox(p))) => p,
                Ok(None) => return Ok(ConfirmRejectOutcome::PayloadInvalid),
                Ok(Some(other)) => {
                    tracing::warn!(
                        message_id = %message_id,
                        variant = other.variant_label(),
                        "reject_controller_box row has unexpected payload variant"
                    );
                    return Ok(ConfirmRejectOutcome::PayloadInvalid);
                }
                Err(e) => {
                    tracing::warn!(message_id = %message_id, error = %e, "reject_controller_box payload deserialize failed");
                    return Ok(ConfirmRejectOutcome::PayloadInvalid);
                }
            };

            if box_payload.is_rejected {
                return Ok(ConfirmRejectOutcome::AlreadyRejected);
            }
            let live_prod_ids = all_live_production_release_ids(conn, project_id)?;
            if assistant_turn_has_live_production_release_with_live_ids(
                conn,
                session_id,
                box_payload.assistant_turn_id,
                &live_prod_ids,
            )? {
                return Ok(ConfirmRejectOutcome::DisabledByLiveProductionRelease);
            }

            let tip: Option<ChatMessageRow> = chat_messages::table
                .filter(chat_messages::chat_session_id.eq(session_id))
                .order((
                    chat_messages::created_at.desc(),
                    chat_messages::id.desc(),
                ))
                .select(ChatMessageRow::as_select())
                .first(conn)
                .optional()?;
            let Some(tip) = tip else {
                return Ok(ConfirmRejectOutcome::NotLastMessage);
            };
            if tip.id != message_id {
                return Ok(ConfirmRejectOutcome::NotLastMessage);
            }

            let unit_ids: Vec<String> = box_payload
                .unit_changes
                .iter()
                .map(|u| u.id.clone())
                .collect();

            let turn_id = box_payload.assistant_turn_id;
            let mut stage_release_id_to_delete: Option<Uuid> = None;
            let mut prod_release_id_to_delete: Option<Uuid> = None;
            let mut stage_runtime_ids_cleared: Vec<Uuid> = Vec::new();
            for u in &mut box_payload.unit_changes {
                u.is_rejected = true;
            }
            box_payload.is_rejected = true;
            let payload_json = match serde_json::to_value(&box_payload) {
                Ok(v) => v,
                Err(_) => return Ok(ConfirmRejectOutcome::PayloadInvalid),
            };

            if !unit_ids.is_empty() {
                diesel::delete(
                    project_units::table
                        .filter(project_units::project_id.eq(project_id))
                        .filter(project_units::chat_session_id.eq(session_id))
                        .filter(project_units::id.eq_any(&unit_ids)),
                )
                .execute(conn)?;
            }

            let reject_row = diesel::update(
                chat_messages::table.filter(chat_messages::id.eq(message_id)),
            )
            .set(chat_messages::payload.eq(Some(payload_json)))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)?;

            let release_row = match turn_id {
                Some(tid) => {
                    let rows: Vec<ChatMessageRow> = chat_messages::table
                        .filter(chat_messages::chat_session_id.eq(session_id))
                        .filter(chat_messages::type_.eq(ChatMessageType::ReleaseControllerBox))
                        .select(ChatMessageRow::as_select())
                        .load(conn)?;
                    let mut updated: Option<ChatMessageRow> = None;
                    for row in rows {
                        let Ok(Some(ChatMessagePayload::ReleaseControllerBox(mut p))) =
                            ChatMessagePayload::from_value(&row.type_, row.payload.clone())
                        else {
                            continue;
                        };
                        if p.assistant_turn_id != Some(tid) {
                            continue;
                        }
                        stage_release_id_to_delete = p.stage_release.as_ref().map(|s| s.release.id);
                        if let Some(pr) = p.prod_release.as_ref()
                            && !live_prod_ids.contains(&pr.release.id) {
                                prod_release_id_to_delete = Some(pr.release.id);
                            }
                        p.stage_release = None;
                        p.prod_release = None;
                        p.is_disabled_due_to_reject = true;
                        let payload_json = match serde_json::to_value(&p) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let updated_row = diesel::update(
                            chat_messages::table.filter(chat_messages::id.eq(row.id)),
                        )
                        .set(chat_messages::payload.eq(Some(payload_json)))
                        .returning(ChatMessageRow::as_returning())
                        .get_result(conn)?;
                        updated = Some(updated_row);
                        break;
                    }
                    updated
                }
                None => None,
            };

            if let Some(stage_release_id) = stage_release_id_to_delete {
                stage_runtime_ids_cleared = project_runtimes::table
                    .filter(project_runtimes::project_id.eq(project_id))
                    .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Stage))
                    .filter(project_runtimes::live_release_id.eq(Some(stage_release_id)))
                    .select(project_runtimes::id)
                    .load::<Uuid>(conn)?;

                if !stage_runtime_ids_cleared.is_empty() {
                    diesel::update(
                        project_runtimes::table
                            .filter(project_runtimes::project_id.eq(project_id))
                            .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Stage))
                            .filter(project_runtimes::live_release_id.eq(Some(stage_release_id))),
                    )
                    .set(project_runtimes::live_release_id.eq(None::<Uuid>))
                    .execute(conn)?;
                }

                diesel::delete(
                    project_releases::table
                        .filter(project_releases::project_id.eq(project_id))
                        .filter(project_releases::id.eq(stage_release_id))
                        .filter(project_releases::is_stage.eq(true)),
                )
                .execute(conn)?;
            }

            if let Some(prod_id) = prod_release_id_to_delete {
                diesel::delete(
                    project_releases::table
                        .filter(project_releases::project_id.eq(project_id))
                        .filter(project_releases::id.eq(prod_id))
                        .filter(project_releases::is_stage.eq(false)),
                )
                .execute(conn)?;
            }

            Ok(ConfirmRejectOutcome::Success(
                reject_row,
                release_row,
                stage_release_id_to_delete,
                stage_runtime_ids_cleared,
            ))
        })
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::error!("[handle_confirm_reject_controller] DB error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        ConfirmRejectOutcome::Success(
            reject_row,
            release_row_opt,
            stage_release_id_opt,
            stage_runtime_ids_cleared,
        ) => {
            if let Some(ref redis_conn) = context.redis_conn {
                let message: ChatMessageResponse = reject_row.into();
                let event =
                    WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
                        project_id,
                        session_id,
                        message,
                    });
                let mut guard = crate::services::redis_conn::clone_connection(redis_conn.clone());
                if let Err(e) =
                    publish_event_to_channel(&mut guard, &user_channel(user_id), &event).await
                {
                    tracing::warn!(
                        "[handle_confirm_reject_controller] WS publish (reject) failed: {}",
                        e
                    );
                }
                if let Some(release_row) = release_row_opt {
                    let message_r: ChatMessageResponse = release_row.into();
                    let event_r = WsServerEvent::ChatSessionMessageUpdated(
                        ChatSessionMessageUpdatedPayload {
                            project_id,
                            session_id,
                            message: message_r,
                        },
                    );
                    if let Err(e) =
                        publish_event_to_channel(&mut guard, &user_channel(user_id), &event_r)
                            .await
                    {
                        tracing::warn!(
                            "[handle_confirm_reject_controller] WS publish (release) failed: {}",
                            e
                        );
                    }
                }
            }

            if context.settings.runtime_nats_url.is_some()
                && let (Some(stage_release_id), true) =
                    (stage_release_id_opt, !stage_runtime_ids_cleared.is_empty())
            {
                let nats = match runtime_nats_required_publisher(context.get_ref()) {
                    Ok(nats) => nats,
                    Err(e) => return runtime_nats_unavailable_response(&e),
                };
                let subject = context.settings.runtime_nats_subject_release.as_str();
                for runtime_id in stage_runtime_ids_cleared {
                    if let Err(e) = nats
                        .publish_stage_release_cleared(
                            subject,
                            project_id,
                            runtime_id,
                            Some(stage_release_id),
                        )
                        .await
                    {
                        return runtime_nats_unavailable_response(&e);
                    }
                }
            }
            HttpResponse::Ok().json(api_success("Reject confirmed", json!({})))
        }
        ConfirmRejectOutcome::SessionNotFound => {
            HttpResponse::NotFound().json(api_error("Chat session not found", json!({})))
        }
        ConfirmRejectOutcome::MessageNotFound => {
            HttpResponse::NotFound().json(api_error("Message not found", json!({})))
        }
        ConfirmRejectOutcome::NotRejectController => HttpResponse::BadRequest().json(api_error(
            "Message is not a reject_controller_box row",
            json!({}),
        )),
        ConfirmRejectOutcome::PayloadInvalid => HttpResponse::BadRequest().json(api_error(
            "Reject controller payload is missing or invalid",
            json!({}),
        )),
        ConfirmRejectOutcome::AlreadyRejected => HttpResponse::BadRequest()
            .json(api_error("Reject controller already confirmed", json!({}))),
        ConfirmRejectOutcome::DisabledByLiveProductionRelease => {
            HttpResponse::BadRequest().json(api_error(
                "Reject controller is disabled because a live production release exists for this turn",
                json!({}),
            ))
        }
        ConfirmRejectOutcome::NotLastMessage => HttpResponse::BadRequest().json(api_error(
            "Reject controller must be the latest message in the chat session",
            json!({}),
        )),
    }
}

pub async fn handle_truncate_chat_session_from_user_message(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid, Uuid)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");
    let user_id = access_payload.user_id;
    let (project_id, session_id, user_message_id) = path.into_inner();

    if let Err(e) = ensure_runtime_nats_available(context.get_ref()).await {
        return runtime_nats_unavailable_response(&e);
    }

    let nats_ctx = TruncateNatsContext {
        nats_required: context.settings.runtime_nats_url.is_some(),
        publisher: context.runtime_nats_publisher.as_deref(),
        subject: context.settings.runtime_nats_subject_release.as_str(),
        project_id,
    };

    let outcome = match truncate_session_from_user_message_with_nats(
        &context.pool,
        nats_ctx,
        user_id,
        project_id,
        session_id,
        user_message_id,
    )
    .await
    {
        Ok(o) => o,
        Err(TruncateWithNatsError::Nats(e)) => {
            return runtime_nats_unavailable_response(&e);
        }
        Err(TruncateWithNatsError::Db(e)) => {
            tracing::error!(
                "[handle_truncate_chat_session_from_user_message] DB error: {}",
                e
            );
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        TruncateSessionFromMessageOutcome::Success {
            removed_message_ids,
            ..
        } => HttpResponse::Ok().json(api_success(
            "Chat truncated",
            json!({ "removed_message_ids": removed_message_ids }),
        )),
        TruncateSessionFromMessageOutcome::SessionNotFound => HttpResponse::NotFound()
            .json(api_error("Chat session not found", json!({}))),
        TruncateSessionFromMessageOutcome::AnchorNotFound => {
            HttpResponse::NotFound().json(api_error("Message not found", json!({})))
        }
        TruncateSessionFromMessageOutcome::AnchorNotUserMessage => HttpResponse::BadRequest()
            .json(api_error("Message is not a user message", json!({}))),
        TruncateSessionFromMessageOutcome::HasLiveProductionReleaseAfterAnchor => {
            HttpResponse::BadRequest().json(api_error(
                "Cannot truncate: a live production release exists on a release controller after this message",
                json!({}),
            ))
        }
    }
}
