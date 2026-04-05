use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::libs::unit_slug::{normalize_full_unit_id_arg, validate_full_unit_id_for_tool};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::{ProjectUnitResponse, ProjectUnitRow};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::{chat_messages, project_units};
use crate::services::chat_worker::db::load_units_from_latest_production_metadata;
use crate::services::message_queue::SharedRabbitPublisher;
use crate::services::unit_technical_reviewer::queue::{
    publish_reviewer_message, UnitReviewerMessageType, UnitReviewerQueueMessage,
};
use diesel::prelude::*;
use gemini_rust::tools::model::FunctionDeclaration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::modify_unit::ARG_UNIT_ID_DESCRIPTION;
use super::{maybe_complete_invoking_message, BoxError, ToolSpec, UnitSubTaskStatus};

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
        pre_check_sub_task: UnitSubTaskStatus::Waiting,
        rule_check_sub_task: UnitSubTaskStatus::Waiting,
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
    rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
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

    let precheck_fmt_error: Option<String> = if parent_key.is_empty() {
        Some("remove_unit: unit_id must not be empty".to_string())
    } else {
        validate_full_unit_id_for_tool(&parent_key)
            .err()
            .map(|e| format!("remove_unit: unit_id invalid: {e}"))
    };

    let mut pre_check_sub_task = UnitSubTaskStatus::Waiting;

    // Pre-check: validate unit_id format, find the parent row, verify it is not already deleted.
    // The actual deletion marker insert is NOT done here; it happens only after the reviewer's
    // rule_check passes (in the reviewer's remove_unit_rule_check success branch).
    let (parent_opt, pre_check_error): (Option<ProjectUnitRow>, Option<String>) =
        if let Some(msg) = precheck_fmt_error {
            pre_check_sub_task = UnitSubTaskStatus::Failed;
            (None, Some(msg))
        } else {
            let pk = parent_key.clone();
            match block_on_db_tokio(&pool, move |conn| {
                let parent: Option<ProjectUnitRow> = project_units::table
                    .filter(project_units::project_id.eq(project_id))
                    .filter(project_units::chat_session_id.eq(session_id))
                    .filter(project_units::id.eq(&pk))
                    .select(ProjectUnitRow::as_select())
                    .first(conn)
                    .optional()?;
                if let Some(p) = parent {
                    return Ok(Some(p));
                }
                let prod_units = load_units_from_latest_production_metadata(conn, project_id)?;
                Ok(prod_units.into_iter().find(|u| u.id == pk && !u.is_deleted).map(|u| u.row))
            })
            .await
            {
                Ok(Some(parent)) if parent.is_deleted => {
                    pre_check_sub_task = UnitSubTaskStatus::Failed;
                    (None, Some("remove_unit: this unit revision is already marked deleted".to_string()))
                }
                Ok(Some(parent)) => {
                    pre_check_sub_task = UnitSubTaskStatus::Succeeded;
                    (Some(parent), None)
                }
                Ok(None) => {
                    pre_check_sub_task = UnitSubTaskStatus::Failed;
                    (
                        None,
                        Some("remove_unit: unit not found in this chat session or the live production release".to_string()),
                    )
                }
                Err(e) => {
                    pre_check_sub_task = UnitSubTaskStatus::Failed;
                    tracing::error!(%message_id, "remove_unit find parent: {}", e);
                    (None, Some(e.to_string()))
                }
            }
        };

    if pre_check_sub_task == UnitSubTaskStatus::Failed {
        // Pre-check failed: save Completed with the error and trigger LLM continuation.
        let full_payload = ToolRemoveUnitPayload {
            args,
            removed_unit: None,
            pre_check_sub_task,
            rule_check_sub_task: UnitSubTaskStatus::Waiting,
            error: pre_check_error,
            tool_call_id,
            tool_name,
        };
        let payload_value = serde_json::to_value(&full_payload)
            .map_err(|e| -> BoxError { format!("serialize remove_unit pre_check failure: {}", e).into() })?;
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
        let invoking_id = updated_row.invoking_message_id;
        let channel = user_channel(user_id);
        let message: ChatMessageResponse = updated_row.into();
        let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
            project_id,
            session_id,
            message,
        });
        {
            let mut guard = redis_conn.lock().await;
            publish_event_to_channel(&mut *guard, &channel, &event)
                .await
                .map_err(|e| -> BoxError { e.to_string().into() })?;
        }
        // ToolRemoveUnit skips generic on_tool_message_finished; call maybe_complete_invoking_message
        // here explicitly because the message is Completed and the LLM must continue.
        if let (Some(inv_id), Some(ref publisher)) = (invoking_id, rabbit_publisher.as_ref()) {
            maybe_complete_invoking_message(
                &pool, &redis_conn, Some(publisher), inv_id, user_id, project_id, session_id, true,
            )
            .await;
        }
        return Ok(());
    }

    // Pre-check succeeded: parent found and not deleted. Save partial state (Generating) and
    // dispatch remove_unit_rule_check directly to the reviewer (remove has no compiler check).
    // removed_unit stays None until the reviewer inserts the deletion marker after rule_check passes.
    let parent = parent_opt.unwrap(); // safe: pre_check succeeded
    let partial_payload = ToolRemoveUnitPayload {
        args: args.clone(),
        removed_unit: None,
        pre_check_sub_task: UnitSubTaskStatus::Succeeded,
        rule_check_sub_task: UnitSubTaskStatus::Waiting,
        error: None,
        tool_call_id,
        tool_name,
    };
    let payload_value = serde_json::to_value(&partial_payload)
        .map_err(|e| -> BoxError { format!("serialize remove_unit partial payload: {}", e).into() })?;
    let updated_row = block_on_db_tokio(&pool, move |conn| {
        diesel::update(chat_messages::table.filter(chat_messages::id.eq(message_id)))
            .set((
                chat_messages::payload.eq(Some(payload_value)),
                chat_messages::status.eq(ChatMessageStatus::Generating),
            ))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
    })
    .await
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;

    let channel = user_channel(user_id);
    let updated_message: ChatMessageResponse = updated_row.into();
    let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
        project_id,
        session_id,
        message: updated_message,
    });
    {
        let mut guard = redis_conn.lock().await;
        publish_event_to_channel(&mut *guard, &channel, &event)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
        // ProjectUnitDeleted is NOT broadcast here. The reviewer broadcasts it only after the
        // deletion marker row is successfully inserted (rule_check success path).
    }

    // remove_unit has no compiler check — dispatch directly to rule_check.
    if let Some(publisher) = rabbit_publisher {
        let reviewer_message = UnitReviewerQueueMessage {
            message_type: UnitReviewerMessageType::RemoveUnitRuleCheck,
            tool_message_id: message_id,
            user_id,
            project_id,
            session_id,
            code: String::new(), // no code for removals
            unit_type: parent.unit_type.as_slug().to_string(),
            context_slug: parent.context_slug.clone(),
            module_slug: parent.module_slug.clone(),
            unit_name_slug: parent.unit_name_slug.clone(),
        };
        publish_reviewer_message(&publisher, &reviewer_message)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
        tracing::info!(
            target: "baal_backend_actix::tool_remove_unit",
            %message_id,
            %project_id,
            %session_id,
            "remove_unit: dispatched remove_unit_rule_check to reviewer queue; tool message stays Generating"
        );
    } else {
        tracing::warn!(
            target: "baal_backend_actix::tool_remove_unit",
            %message_id,
            "remove_unit: no rabbit_publisher available; cannot dispatch to reviewer queue"
        );
    }

    Ok(())
}
