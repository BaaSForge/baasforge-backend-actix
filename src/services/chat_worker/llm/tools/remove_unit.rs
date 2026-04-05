use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::libs::unit_slug::{compose_unit_id, normalize_full_unit_id_arg, validate_full_unit_id_for_tool};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::{ProjectUnitInsert, ProjectUnitResponse, ProjectUnitRow};
use crate::routes::ws::events::{
    ChatSessionMessageUpdatedPayload, ProjectUnitEventPayload, WsServerEvent,
};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::{chat_messages, project_units};
use crate::services::chat_worker::db::load_units_from_latest_production_metadata;
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
use super::{BoxError, ToolSpec, UnitSubTaskStatus};

/// LLM JSON shape for `remove_unit`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct RemoveUnitLlmArgs {
    #[schemars(description = ARG_UNIT_ID_DESCRIPTION)]
    unit_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRemoveUnitArgs {
    pub unit_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRemoveUnitPayload {
    pub args: ToolRemoveUnitArgs,
    pub removed_unit: Option<ProjectUnitResponse>,
    /// Internal sub-task status for client UI. Never sent to the LLM in history.
    pub pre_check_sub_task: UnitSubTaskStatus,
    /// Internal sub-task status for client UI. Never sent to the LLM in history.
    pub rule_check_sub_task: UnitSubTaskStatus,
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
    let unit_id = normalize_full_unit_id_arg(&raw.unit_id);
    let args = ToolRemoveUnitArgs { unit_id };
    Ok(ChatMessagePayload::ToolRemoveUnit(ToolRemoveUnitPayload {
        args,
        removed_unit: None,
        pre_check_sub_task: UnitSubTaskStatus::Succeeded,
        rule_check_sub_task: UnitSubTaskStatus::Succeeded,
        error: None,
        tool_call_id,
        tool_name: Some(tool_name.to_string()),
    }))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "unit_id": { "type": "string", "description": ARG_UNIT_ID_DESCRIPTION }
        },
        "required": ["unit_id"]
    })
}

pub const TOOL_NAME: &str = "remove_unit";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Soft-remove a unit by inserting a new draft revision: empty code, explanation copied, is_deleted true, new `id` with last segment revision+1, same `chat_session_id` and slugs as the parent row. `unit_id` must be the full id with revision as digits only in the last segment (no `v`/`rev` prefix).",
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
    payload: Option<ChatMessagePayload>,
    _rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let existing = match payload {
        Some(ChatMessagePayload::ToolRemoveUnit(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .ok_or_else(|| -> BoxError { "remove_unit: missing payload".into() })?;
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());

    let parent_key = args.unit_id.clone();

    let precheck: Option<String> = if parent_key.is_empty() {
        Some("remove_unit: unit_id must not be empty".to_string())
    } else {
        validate_full_unit_id_for_tool(&parent_key)
            .err()
            .map(|e| format!("remove_unit: unit_id invalid: {e}"))
    };

    let result = if let Some(msg) = precheck {
        Ok(Err(msg))
    } else {
        block_on_db_tokio(&pool, move |conn| {
        let parent: Option<ProjectUnitRow> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(session_id))
            .filter(project_units::id.eq(&parent_key))
            .select(ProjectUnitRow::as_select())
            .first(conn)
            .optional()?;

        let parent = match parent {
            Some(p) => p,
            None => {
                let prod_units = load_units_from_latest_production_metadata(conn, project_id)?;
                match prod_units.into_iter().find(|u| u.id == parent_key && !u.is_deleted) {
                    Some(prod_unit) => prod_unit.row,
                    None => return Ok(Err(
                        "remove_unit: unit not found in this chat session or the live production release".to_string(),
                    )),
                }
            }
        };

        if parent.is_deleted {
            return Ok(Err(
                "remove_unit: this unit revision is already marked deleted".to_string(),
            ));
        }
        let new_rev = parent.revision + 1;
        let new_id = compose_unit_id(
            &parent.context_slug,
            &parent.module_slug,
            parent.unit_type.as_slug(),
            &parent.unit_name_slug,
            new_rev,
        );
        let insert = ProjectUnitInsert {
            project_id,
            chat_session_id: session_id,
            id: new_id,
            revision: new_rev,
            code: Some(String::new()),
            explanation: parent.explanation.clone(),
            unit_type: parent.unit_type,
            is_deleted: true,
            context_slug: parent.context_slug.clone(),
            module_slug: parent.module_slug.clone(),
            unit_name_slug: parent.unit_name_slug.clone(),
        };
        let row = diesel::insert_into(project_units::table)
            .values(&insert)
            .returning(ProjectUnitRow::as_returning())
            .get_result::<ProjectUnitRow>(conn)?;
        Ok(Ok(row))
    })
        .await
    };

    let (removed_unit, error): (Option<ProjectUnitResponse>, Option<String>) = match result {
        Ok(Ok(u)) => {
            let rev = u.revision;
            (Some(ProjectUnitResponse::from_row(u, rev)), None)
        }
        Ok(Err(msg)) => (None, Some(msg)),
        Err(e) => {
            tracing::error!(%message_id, "remove_unit: {}", e);
            (None, Some(e.to_string()))
        }
    };
    let unit_for_ws = removed_unit.clone();

    let full_payload = ToolRemoveUnitPayload {
        args,
        removed_unit,
        pre_check_sub_task: UnitSubTaskStatus::Succeeded,
        rule_check_sub_task: UnitSubTaskStatus::Succeeded,
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
    if let Some(unit) = unit_for_ws {
        let unit_event = WsServerEvent::ProjectUnitDeleted(ProjectUnitEventPayload {
            project_id,
            session_id,
            unit,
        });
        publish_event_to_channel(&mut *guard, &channel, &unit_event)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
    }
    Ok(())
}
