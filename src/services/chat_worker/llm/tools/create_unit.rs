use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::libs::unit_slug::{compose_unit_id, is_valid_slug};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::{ProjectUnitInsert, ProjectUnitResponse, ProjectUnitType};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::{chat_messages, project_units};
use crate::services::chat_worker::units_definitions;
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

/// Description for the `explanation` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_EXPLANATION_DESCRIPTION: &str =
    "English explanation of what this unit does (purpose, boundaries, how others use it).";
/// Description for the `code` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_CODE_DESCRIPTION: &str =
    "Implementation for this unit in JavaScript.";
pub const ARG_CONTEXT_SLUG_DESCRIPTION: &str =
    "Bounded context slug: only English lowercase letters and hyphen (-), e.g. inventory.";
pub const ARG_MODULE_SLUG_DESCRIPTION: &str =
    "Module slug within the context: only English lowercase letters and hyphen (-), e.g. stock-levels.";
pub const ARG_UNIT_NAME_SLUG_DESCRIPTION: &str =
    "Unit name slug describing what this unit does: only English lowercase letters and hyphen (-), e.g. adjust-levels.";

const INITIAL_REVISION: i32 = 1;

/// LLM JSON shape for `create_unit`; `unit_type` is parsed to [`ProjectUnitType`] in [`payload_from_args`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CreateUnitLlmArgs {
    #[schemars(description = ARG_CONTEXT_SLUG_DESCRIPTION)]
    context_slug: String,
    #[schemars(description = ARG_MODULE_SLUG_DESCRIPTION)]
    module_slug: String,
    #[schemars(description = ARG_UNIT_NAME_SLUG_DESCRIPTION)]
    unit_name_slug: String,
    #[schemars(description = ARG_EXPLANATION_DESCRIPTION)]
    explanation: String,
    #[schemars(description = ARG_CODE_DESCRIPTION)]
    code: String,
    #[schemars(description = units_definitions::UNIT_TYPE_ARG_DESCRIPTION)]
    unit_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCreateUnitArgs {
    pub context_slug: String,
    pub module_slug: String,
    pub unit_name_slug: String,
    pub explanation: String,
    pub code: String,
    pub unit_type: ProjectUnitType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCreateUnitPayload {
    pub args: ToolCreateUnitArgs,
    pub created_unit: Option<ProjectUnitResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<CreateUnitLlmArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolCreateUnit;

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let raw = serde_json::from_value::<CreateUnitLlmArgs>(args)
        .map_err(|e| -> BoxError { format!("create_unit args: {}", e).into() })?;
    let unit_type: ProjectUnitType = serde_json::from_value(json!(raw.unit_type.trim()))
        .map_err(|e| -> BoxError { format!("create_unit unit_type: {}", e).into() })?;
    let context_slug = raw.context_slug.trim().to_string();
    let module_slug = raw.module_slug.trim().to_string();
    let unit_name_slug = raw.unit_name_slug.trim().to_string();
    if !is_valid_slug(&context_slug) {
        return Err("create_unit: context_slug must be only English lowercase letters and hyphens (-)".into());
    }
    if !is_valid_slug(&module_slug) {
        return Err("create_unit: module_slug must be only English lowercase letters and hyphens (-)".into());
    }
    if !is_valid_slug(&unit_name_slug) {
        return Err("create_unit: unit_name_slug must be only English lowercase letters and hyphens (-)".into());
    }
    let args = ToolCreateUnitArgs {
        context_slug,
        module_slug,
        unit_name_slug,
        explanation: raw.explanation,
        code: raw.code,
        unit_type,
    };
    Ok(ChatMessagePayload::ToolCreateUnit(ToolCreateUnitPayload {
        args,
        created_unit: None,
        error: None,
        tool_call_id,
        tool_name: Some(tool_name.to_string()),
    }))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "context_slug": { "type": "string", "description": ARG_CONTEXT_SLUG_DESCRIPTION },
            "module_slug": { "type": "string", "description": ARG_MODULE_SLUG_DESCRIPTION },
            "unit_name_slug": { "type": "string", "description": ARG_UNIT_NAME_SLUG_DESCRIPTION },
            "explanation": { "type": "string", "description": ARG_EXPLANATION_DESCRIPTION },
            "code": { "type": "string", "description": ARG_CODE_DESCRIPTION },
            "unit_type": {
                "type": "string",
                "enum": units_definitions::UNIT_KIND_SLUGS,
                "description": units_definitions::unit_type_description_for_tools()
            }
        },
        "required": ["context_slug", "module_slug", "unit_name_slug", "explanation", "code", "unit_type"]
    })
}

pub const TOOL_NAME: &str = "create_unit";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Create a new draft unit for this project. Provide context_slug, module_slug, and unit_name_slug (English lowercase and hyphens only). The row `id` is those parts and unit kind, dot-separated, with revision as the last segment (1 for a new unit). The `code` argument must be JavaScript. Inserts a new row with is_draft true.",
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
        .and_then(|v| serde_json::from_value::<ToolCreateUnitPayload>(v.clone()).ok());
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .ok_or_else(|| -> BoxError { "create_unit: missing payload".into() })?;
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());

    let id = compose_unit_id(
        &args.context_slug,
        &args.module_slug,
        args.unit_type.as_slug(),
        &args.unit_name_slug,
        INITIAL_REVISION,
    );
    let insert = ProjectUnitInsert {
        project_id,
        chat_session_id: session_id,
        id,
        revision: INITIAL_REVISION,
        code: Some(args.code.clone()),
        explanation: Some(args.explanation.clone()),
        unit_type: args.unit_type.clone(),
        is_draft: true,
        is_deleted: false,
        context_slug: args.context_slug.clone(),
        module_slug: args.module_slug.clone(),
        unit_name_slug: args.unit_name_slug.clone(),
    };

    let created = block_on_db_tokio(&pool, move |conn| {
        diesel::insert_into(project_units::table)
            .values(&insert)
            .returning(ProjectUnitResponse::as_returning())
            .get_result::<ProjectUnitResponse>(conn)
    })
    .await;

    let (created_unit, error) = match created {
        Ok(u) => (Some(u), None),
        Err(e) => {
            tracing::error!(%message_id, "create_unit insert: {}", e);
            (None, Some(e.to_string()))
        }
    };

    let full_payload = ToolCreateUnitPayload {
        args,
        created_unit,
        error,
        tool_call_id,
        tool_name,
    };
    let payload_value = serde_json::to_value(&full_payload)
        .map_err(|e| -> BoxError { format!("serialize create_unit result: {}", e).into() })?;
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
