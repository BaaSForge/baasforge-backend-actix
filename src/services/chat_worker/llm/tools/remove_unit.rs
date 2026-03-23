use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::libs::ids::new_uuid;
use crate::libs::semver::bump_major_version;
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::{ProjectUnitInsert, ProjectUnitResponse};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::{chat_messages, project_units};
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use gemini_rust::tools::model::FunctionDeclaration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::modify_unit::ARG_UNIT_ID_DESCRIPTION;
use super::{BoxError, ToolSpec};

/// LLM JSON shape for `remove_unit`. Unit kind and new version are derived in the database.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct RemoveUnitLlmArgs {
    #[schemars(description = ARG_UNIT_ID_DESCRIPTION)]
    unit_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRemoveUnitArgs {
    pub unit_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRemoveUnitPayload {
    pub args: ToolRemoveUnitArgs,
    pub removed_unit: Option<ProjectUnitResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None).with_parameters::<RemoveUnitLlmArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolRemoveUnit;

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let raw = serde_json::from_value::<RemoveUnitLlmArgs>(args)
        .map_err(|e| -> BoxError { format!("remove_unit args: {}", e).into() })?;
    let unit_id: Uuid = raw
        .unit_id
        .trim()
        .parse()
        .map_err(|e| -> BoxError { format!("remove_unit unit_id: {}", e).into() })?;
    let args = ToolRemoveUnitArgs { unit_id };
    Ok(ChatMessagePayload::ToolRemoveUnit(ToolRemoveUnitPayload {
        args,
        removed_unit: None,
        error: None,
        tool_call_id,
        tool_name: Some(tool_name.to_string()),
    }))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "unit_id": { "type": "string", "format": "uuid", "description": ARG_UNIT_ID_DESCRIPTION }
        },
        "required": ["unit_id"]
    })
}

pub const TOOL_NAME: &str = "remove_unit";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Mark a unit as removed without deleting its history. Inserts a new draft row with empty code, the parent unit’s explanation preserved, is_deleted true, semantic version bumped by one major (breaking), same unit kind as the parent, parent_unit_id pointing at the unit you pass, and is_draft true.",
        parameters: params_schema(),
    }
}

pub async fn handler(
    pool: DbPool,
    redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    payload: Option<Value>,
    _rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let existing = payload
        .as_ref()
        .and_then(|v| serde_json::from_value::<ToolRemoveUnitPayload>(v.clone()).ok());
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .ok_or_else(|| -> BoxError { "remove_unit: missing payload".into() })?;
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());

    let parent_id = args.unit_id;
    let new_id = new_uuid();

    let result = block_on_db_tokio(&pool, move |conn| {
        let parent: Option<ProjectUnitResponse> = project_units::table
            .filter(project_units::id.eq(parent_id))
            .filter(project_units::project_id.eq(project_id))
            .select(ProjectUnitResponse::as_select())
            .first(conn)
            .optional()?;
        let Some(parent) = parent else {
            return Ok(Err(
                "remove_unit: unit not found for this project".to_string(),
            ));
        };
        if parent.is_deleted {
            return Ok(Err(
                "remove_unit: this unit revision is already marked deleted".to_string(),
            ));
        }
        let version = match bump_major_version(&parent.version) {
            Ok(v) => v,
            Err(msg) => return Ok(Err(format!("remove_unit: {}", msg))),
        };
        let insert = ProjectUnitInsert {
            id: new_id,
            project_id,
            code: Some(String::new()),
            explanation: parent.explanation.clone(),
            version,
            parent_unit_id: Some(parent_id),
            unit_type: parent.unit_type,
            is_draft: true,
            chat_session_id: Some(session_id),
            is_deleted: true,
        };
        let row = diesel::insert_into(project_units::table)
            .values(&insert)
            .returning(ProjectUnitResponse::as_returning())
            .get_result::<ProjectUnitResponse>(conn)?;
        Ok(Ok(row))
    })
    .await;

    let (removed_unit, error): (Option<ProjectUnitResponse>, Option<String>) = match result {
        Ok(Ok(u)) => (Some(u), None),
        Ok(Err(msg)) => (None, Some(msg)),
        Err(e) => {
            tracing::error!(%message_id, "remove_unit: {}", e);
            (None, Some(e.to_string()))
        }
    };

    let full_payload = ToolRemoveUnitPayload {
        args,
        removed_unit,
        error,
        tool_call_id,
        tool_name,
    };
    let payload_value = serde_json::to_value(&full_payload)
        .map_err(|e| -> BoxError { format!("serialize remove_unit result: {}", e).into() })?;
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
    let mut guard = redis_conn.lock().await;
    publish_event_to_channel(&mut *guard, &channel, &event)
        .await
        .map_err(|e| -> BoxError { e.to_string().into() })?;
    Ok(())
}
