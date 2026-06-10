use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::libs::unit_slug::{
    LLM_UNIT_ID_TOOL_DESCRIPTION, normalize_full_unit_id_arg, validate_full_unit_id_for_tool,
};
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
    UnitReviewerMessageType, UnitReviewerQueueMessage, publish_reviewer_message,
};
use crate::services::units::HttpMethod;
use diesel::prelude::*;
use gemini_rust::tools::model::FunctionDeclaration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{BoxError, ToolSpec, UnitSubTaskStatus, maybe_complete_invoking_message};

pub const ARG_UNIT_ID_DESCRIPTION: &str = LLM_UNIT_ID_TOOL_DESCRIPTION;
/// Description for the `explanation` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_EXPLANATION_DESCRIPTION: &str =
    "English explanation of what this unit does (purpose, boundaries, how others use it).";
/// Description for the `code` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_CODE_DESCRIPTION: &str =
    "Implementation for this unit in TypeScript (Node.js-like environment).";

/// LLM JSON shape for `modify_unit`. Unit kind and slugs are taken from the parent row in the database.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ModifyUnitLlmArgs {
    #[schemars(description = ARG_UNIT_ID_DESCRIPTION)]
    unit_id: String,
    #[schemars(description = ARG_EXPLANATION_DESCRIPTION)]
    explanation: String,
    #[schemars(description = ARG_CODE_DESCRIPTION)]
    code: String,
    #[schemars(
        description = "HTTP method for API units (GET, POST, PUT, PATCH, DELETE, OPTIONS, HEAD). Only when modifying an api unit to change the method; omit to leave unchanged."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_method: Option<HttpMethod>,
    #[schemars(
        description = "Optional timeout in seconds for API units. Only when modifying an api unit; omit to leave timeout unchanged."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolModifyUnitArgs {
    pub unit_id: String,
    pub explanation: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_method: Option<HttpMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolModifyUnitPayload {
    pub args: ToolModifyUnitArgs,
    pub revised_unit: Option<ProjectUnitResponse>,
    /// Internal sub-task status for client UI. Never sent to the LLM in history.
    pub pre_check_sub_task: UnitSubTaskStatus,
    /// Internal sub-task status for client UI. Never sent to the LLM in history.
    pub compiler_check_sub_task: UnitSubTaskStatus,
    #[serde(default)]
    pub lint_check_sub_task: UnitSubTaskStatus,
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
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ModifyUnitLlmArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolModifyUnit;

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let raw = serde_json::from_value::<ModifyUnitLlmArgs>(args)
        .map_err(|e| -> BoxError { format!("modify_unit args: {}", e).into() })?;
    let unit_id = normalize_full_unit_id_arg(&raw.unit_id);
    let args = ToolModifyUnitArgs {
        unit_id,
        explanation: raw.explanation,
        code: raw.code,
        api_method: raw.api_method,
        api_timeout_seconds: raw.api_timeout_seconds,
    };
    Ok(ChatMessagePayload::ToolModifyUnit(ToolModifyUnitPayload {
        args,
        revised_unit: None,
        pre_check_sub_task: UnitSubTaskStatus::Waiting,
        compiler_check_sub_task: UnitSubTaskStatus::Waiting,
        lint_check_sub_task: UnitSubTaskStatus::Waiting,
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
            "unit_id": { "type": "string", "description": ARG_UNIT_ID_DESCRIPTION },
            "explanation": { "type": "string", "description": ARG_EXPLANATION_DESCRIPTION },
            "code": { "type": "string", "description": ARG_CODE_DESCRIPTION },
            "api_method": {
                "type": "string",
                "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"],
                "description": "HTTP method for API units. Only set when modifying an api unit and you want to change its HTTP method. Omit to keep the existing method unchanged."
            },
            "api_timeout_seconds": {
                "type": "integer",
                "minimum": 1,
                "description": "Optional timeout in seconds for API units. Only set when modifying an api unit to change the timeout. Omit to keep the existing value."
            }
        },
        "required": ["unit_id", "explanation", "code"]
    })
}

pub const TOOL_NAME: &str = "modify_unit";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Save a new revision of an existing unit. Parent row is identified by `unit_id` (full dot-separated id; last segment is revision digits only, no `v`/`rev` prefix). Inserts a new row with the same slugs and session, new `id` with last segment revision+1, and `revision` = parent.revision + 1. The `code` argument must be TypeScript (Node.js environment).",
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
    rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let existing = match payload {
        Some(ChatMessagePayload::ToolModifyUnit(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .ok_or_else(|| -> BoxError { "modify_unit: missing payload".into() })?;
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());

    let parent_key = args.unit_id.clone();

    let precheck_fmt_error: Option<String> = if parent_key.is_empty() {
        Some("modify_unit: unit_id must not be empty".to_string())
    } else {
        validate_full_unit_id_for_tool(&parent_key)
            .err()
            .map(|e| format!("modify_unit: unit_id invalid: {e}"))
    };

    let mut pre_check_sub_task = UnitSubTaskStatus::Waiting;

    // Pre-check: validate unit_id format and find the parent row.
    // The actual project_units insert is NOT done here; it happens only after all reviewer checks
    // pass (in the reviewer's modify_unit_rule_check success branch).
    let (parent_opt, pre_check_error): (Option<ProjectUnitRow>, Option<String>) = if let Some(msg) =
        precheck_fmt_error
    {
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
            Ok(prod_units
                .into_iter()
                .find(|u| u.id == pk && !u.is_deleted)
                .map(|u| u.row))
        })
        .await
        {
            Ok(Some(parent)) => {
                pre_check_sub_task = UnitSubTaskStatus::Succeeded;
                (Some(parent), None)
            }
            Ok(None) => {
                pre_check_sub_task = UnitSubTaskStatus::Failed;
                (
                        None,
                        Some("modify_unit: unit not found in this chat session or the live production release".to_string()),
                    )
            }
            Err(e) => {
                pre_check_sub_task = UnitSubTaskStatus::Failed;
                tracing::error!(%message_id, "modify_unit find parent: {}", e);
                (None, Some(e.to_string()))
            }
        }
    };

    if pre_check_sub_task == UnitSubTaskStatus::Failed {
        // Pre-check failed: save Completed with the error and trigger LLM continuation.
        let full_payload = ToolModifyUnitPayload {
            args,
            revised_unit: None,
            pre_check_sub_task,
            compiler_check_sub_task: UnitSubTaskStatus::Waiting,
            lint_check_sub_task: UnitSubTaskStatus::Waiting,
            rule_check_sub_task: UnitSubTaskStatus::Waiting,
            error: pre_check_error,
            tool_call_id,
            tool_name,
        };
        let payload_value = serde_json::to_value(&full_payload).map_err(|e| -> BoxError {
            format!("serialize modify_unit pre_check failure: {}", e).into()
        })?;
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
            crate::routes::ws::redis::publish_event_on_conn(redis_conn.clone(), &channel, &event)
                .await
                .map_err(|e| -> BoxError { e.to_string().into() })?;
        }
        // ToolModifyUnit skips generic on_tool_message_finished; call maybe_complete_invoking_message
        // here explicitly because the message is Completed and the LLM must continue.
        if let (Some(inv_id), Some(publisher)) = (invoking_id, rabbit_publisher.as_ref()) {
            maybe_complete_invoking_message(
                &pool,
                &redis_conn,
                Some(publisher),
                inv_id,
                user_id,
                project_id,
                session_id,
                true,
            )
            .await;
        }
        return Ok(());
    }

    // Pre-check succeeded: parent found. Save partial state (Generating) and dispatch to reviewer.
    // revised_unit stays None until the reviewer's rule_check passes and inserts the new revision.
    let parent = parent_opt.unwrap(); // safe: pre_check succeeded
    let partial_payload = ToolModifyUnitPayload {
        args: args.clone(),
        revised_unit: None,
        pre_check_sub_task: UnitSubTaskStatus::Succeeded,
        compiler_check_sub_task: UnitSubTaskStatus::Waiting,
        lint_check_sub_task: UnitSubTaskStatus::Waiting,
        rule_check_sub_task: UnitSubTaskStatus::Waiting,
        error: None,
        tool_call_id,
        tool_name,
    };
    let payload_value = serde_json::to_value(&partial_payload).map_err(|e| -> BoxError {
        format!("serialize modify_unit partial payload: {}", e).into()
    })?;
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
        crate::routes::ws::redis::publish_event_on_conn(redis_conn.clone(), &channel, &event)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
        // ProjectUnitModified is NOT broadcast here. The reviewer broadcasts it only after the
        // new revision row is successfully inserted (rule_check success path).
    }

    if let Some(publisher) = rabbit_publisher {
        let reviewer_message = UnitReviewerQueueMessage {
            message_type: UnitReviewerMessageType::ModifyUnitCompilerCheck,
            tool_message_id: message_id,
            user_id,
            project_id,
            session_id,
            code: args.code.clone(),
            unit_type: parent.unit_type.as_slug().to_string(),
            context_slug: parent.context_slug.clone(),
            module_slug: parent.module_slug.clone(),
            unit_name_slug: parent.unit_name_slug.clone(),
            retry_num: 0,
        };
        publish_reviewer_message(&publisher, &reviewer_message)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
        tracing::info!(
            target: "baal_backend_actix::tool_modify_unit",
            %message_id,
            %project_id,
            %session_id,
            "modify_unit: dispatched modify_unit_compiler_check to reviewer queue; tool message stays Generating"
        );
    } else {
        tracing::warn!(
            target: "baal_backend_actix::tool_modify_unit",
            %message_id,
            "modify_unit: no rabbit_publisher available; cannot dispatch to reviewer queue"
        );
    }

    Ok(())
}
