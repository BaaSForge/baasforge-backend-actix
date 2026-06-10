use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::ProjectUnitResponse;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::fetch_session_production_units_common::{
    ARG_UNIT_IDS_DESCRIPTION, MAX_UNIT_IDS_OR_PATHS, missing_unit_ids_message,
    normalize_dedupe_unit_ids, validate_unit_ids,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_related_units_for_chat_session_by_full_ids;
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{BoxError, ToolSpec};
use gemini_rust::tools::model::FunctionDeclaration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFetchSpecificProductionReleaseUnitsArgs {
    #[schemars(description = ARG_UNIT_IDS_DESCRIPTION)]
    pub unit_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolFetchSpecificProductionReleaseUnitsPayload {
    pub args: ToolFetchSpecificProductionReleaseUnitsArgs,
    pub units: Vec<ProjectUnitResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolFetchSpecificProductionReleaseUnitsArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType =
    ChatMessageType::ToolFetchSpecificProductionReleaseUnits;

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let mut args =
        serde_json::from_value::<ToolFetchSpecificProductionReleaseUnitsArgs>(args)
            .unwrap_or_default();
    args.unit_ids = normalize_dedupe_unit_ids(&args.unit_ids);
    Ok(ChatMessagePayload::ToolFetchSpecificProductionReleaseUnits(
        ToolFetchSpecificProductionReleaseUnitsPayload {
            args,
            units: vec![],
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "unit_ids": {
                "type": "array",
                "items": { "type": "string", "description": ARG_UNIT_IDS_DESCRIPTION },
                "description": ARG_UNIT_IDS_DESCRIPTION
            }
        },
        "required": ["unit_ids"]
    })
}

pub const TOOL_NAME: &str = "load_specific_production_release_units";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Load specific units by full unit id (including revision). Returns the **exact revision** in each `unit_id`, does **not** substitute a newer revision. Use when you already know the ids (e.g. from a prior tool result or the directive) and you want to know about the history of unit(s), if you want to load the latest revisions of some units, use **load_latest_specific_production_release_units** instead. Pass every target id in `unit_ids` (non-empty).",
        parameters: params_schema(),
    }
}

pub async fn handler(
    pool: DbPool,
    redis_conn: crate::services::redis_conn::RedisConn,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    payload: Option<ChatMessagePayload>,
    _rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let existing = match payload {
        Some(ChatMessagePayload::ToolFetchSpecificProductionReleaseUnits(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .unwrap_or_default();
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());
    let unit_ids = args.unit_ids.clone();

    let (units, error) = if unit_ids.is_empty() {
        (
            vec![],
            Some(format!(
                "{TOOL_NAME}: `unit_ids` is required and must contain at least one full unit id."
            )),
        )
    } else if unit_ids.len() > MAX_UNIT_IDS_OR_PATHS {
        (
            vec![],
            Some(format!(
                "{TOOL_NAME}: at most {MAX_UNIT_IDS_OR_PATHS} unit ids per call; got {}.",
                unit_ids.len()
            )),
        )
    } else if let Err(msg) = validate_unit_ids(&unit_ids) {
        (vec![], Some(format!("{TOOL_NAME}: {msg}")))
    } else {
        let ids_for_db = unit_ids.clone();
        let units = block_on_db_tokio(&pool, move |conn| {
            load_related_units_for_chat_session_by_full_ids(
                conn,
                project_id,
                session_id,
                &ids_for_db,
            )
        })
        .await
        .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;
        let error = missing_unit_ids_message(&unit_ids, &units);
        (units, error)
    };

    let full_payload = ToolFetchSpecificProductionReleaseUnitsPayload {
        args,
        units,
        tool_call_id,
        tool_name,
        error,
    };
    let payload_value = serde_json::to_value(&full_payload)
        .map_err(|e| -> BoxError { format!("serialize units: {}", e).into() })?;
    let updated_row = block_on_db_tokio(&pool, move |conn| {
        diesel::update(chat_messages::table.filter(chat_messages::id.eq(message_id)))
            .set((
                chat_messages::payload.eq(Some(payload_value)),
                chat_messages::status.eq(ChatMessageStatus::Completed),
            ))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
    })
    .await
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;
    let channel = user_channel(user_id);
    let message: ChatMessageResponse = updated_row.into();
    let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
        project_id,
        session_id,
        message,
    });
    crate::routes::ws::redis::publish_event_on_conn(redis_conn.clone(), &channel, &event)
        .await
        .map_err(|e| -> BoxError { e.to_string().into() })?;
    Ok(())
}
