use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::libs::unit_slug::compose_unit_id;
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::{ProjectUnitInsert, ProjectUnitResponse, ProjectUnitRow};
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

use super::{BoxError, ToolSpec};

pub const ARG_UNIT_ID_DESCRIPTION: &str =
    "Exact `id` from list or tool results: context.module.unit_kind.unit_name.revision (dot-separated)—copy the whole string.";
/// Description for the `explanation` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_EXPLANATION_DESCRIPTION: &str =
    "English explanation of what this unit does (purpose, boundaries, how others use it).";
/// Description for the `code` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_CODE_DESCRIPTION: &str =
    "Implementation for this unit in JavaScript.";

/// LLM JSON shape for `modify_unit`. Unit kind and slugs are taken from the parent row in the database.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ModifyUnitLlmArgs {
    #[schemars(description = ARG_UNIT_ID_DESCRIPTION)]
    unit_id: String,
    #[schemars(description = ARG_EXPLANATION_DESCRIPTION)]
    explanation: String,
    #[schemars(description = ARG_CODE_DESCRIPTION)]
    code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolModifyUnitArgs {
    pub unit_id: String,
    pub explanation: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolModifyUnitPayload {
    pub args: ToolModifyUnitArgs,
    pub revised_unit: Option<ProjectUnitResponse>,
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
    let unit_id = raw.unit_id.trim().to_string();
    if unit_id.is_empty() {
        return Err("modify_unit: unit_id must not be empty".into());
    }
    let args = ToolModifyUnitArgs {
        unit_id,
        explanation: raw.explanation,
        code: raw.code,
    };
    Ok(ChatMessagePayload::ToolModifyUnit(ToolModifyUnitPayload {
        args,
        revised_unit: None,
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
            "code": { "type": "string", "description": ARG_CODE_DESCRIPTION }
        },
        "required": ["unit_id", "explanation", "code"]
    })
}

pub const TOOL_NAME: &str = "modify_unit";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Save a new draft revision of an existing unit. Parent row is identified by `unit_id` (full dot-separated id including revision). Inserts a new row with the same slugs and session, new `id` with last segment revision+1, and `revision` = parent.revision + 1. The `code` argument must be JavaScript. Sets is_draft true.",
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
        .and_then(|v| serde_json::from_value::<ToolModifyUnitPayload>(v.clone()).ok());
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .ok_or_else(|| -> BoxError { "modify_unit: missing payload".into() })?;
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());

    let parent_key = args.unit_id.clone();
    let code = args.code.clone();
    let explanation = args.explanation.clone();

    let result = block_on_db_tokio(&pool, move |conn| {
        let parent: Option<ProjectUnitRow> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(session_id))
            .filter(project_units::id.eq(&parent_key))
            .select(ProjectUnitRow::as_select())
            .first(conn)
            .optional()?;
        let Some(parent) = parent else {
            return Ok(Err("modify_unit: unit not found for this project".to_string()));
        };
        let new_id = compose_unit_id(
            &parent.context_slug,
            &parent.module_slug,
            parent.unit_type.as_slug(),
            &parent.unit_name_slug,
            parent.revision + 1,
        );
        let insert = ProjectUnitInsert {
            project_id,
            chat_session_id: parent.chat_session_id,
            id: new_id,
            revision: parent.revision + 1,
            code: Some(code),
            explanation: Some(explanation),
            unit_type: parent.unit_type,
            is_draft: true,
            is_deleted: false,
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
    .await;

    let (revised_unit, error): (Option<ProjectUnitResponse>, Option<String>) = match result {
        Ok(Ok(u)) => {
            let rev = u.revision;
            (Some(ProjectUnitResponse::from_row(u, rev)), None)
        }
        Ok(Err(msg)) => (None, Some(msg)),
        Err(e) => {
            tracing::error!(%message_id, "modify_unit: {}", e);
            (None, Some(e.to_string()))
        }
    };

    let full_payload = ToolModifyUnitPayload {
        args,
        revised_unit,
        error,
        tool_call_id,
        tool_name,
    };
    let payload_value = serde_json::to_value(&full_payload)
        .map_err(|e| -> BoxError { format!("serialize modify_unit result: {}", e).into() })?;
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
