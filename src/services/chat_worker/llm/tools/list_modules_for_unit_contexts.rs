use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::modules_by_context_slugs_visible_tip_units;
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

pub const ARG_CONTEXT_SLUGS_DESCRIPTION: &str =
    "Bounded context slugs to inspect (e.g. inventory-stocks, billing). Pass only contexts you care about; the tool returns distinct module slugs per context in the project. Must be a non-empty array.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct ToolListModulesForUnitContextsArgs {
    #[schemars(description = ARG_CONTEXT_SLUGS_DESCRIPTION)]
    pub context_slugs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextModulesEntry {
    pub context_slug: String,
    pub module_slugs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolListModulesForUnitContextsPayload {
    pub args: ToolListModulesForUnitContextsArgs,
    #[serde(default)]
    pub by_context: Vec<ContextModulesEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolListModulesForUnitContextsArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolListModulesForUnitContexts;

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args: ToolListModulesForUnitContextsArgs = serde_json::from_value(args)
        .map_err(|e| -> BoxError { format!("list_modules_for_unit_contexts: invalid args: {}", e).into() })?;
    if args.context_slugs.iter().all(|s| s.trim().is_empty()) {
        return Err("list_modules_for_unit_contexts: context_slugs must contain at least one non-empty context slug".into());
    }
    Ok(ChatMessagePayload::ToolListModulesForUnitContexts(
        ToolListModulesForUnitContextsPayload {
            args,
            by_context: vec![],
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
        },
    ))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "context_slugs": {
                "type": "array",
                "items": { "type": "string" },
                "description": ARG_CONTEXT_SLUGS_DESCRIPTION
            }
        },
        "required": ["context_slugs"]
    })
}

pub const TOOL_NAME: &str = "list_modules_for_unit_contexts";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "For each bounded context slug you pass, list the distinct module slugs (`module_slug`) used by units in that context. Use after you know which contexts matter (e.g. from list_unit_contexts) to place new units next to existing modules or avoid redundant module names.",
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
    let parsed: ToolListModulesForUnitContextsPayload = match payload {
        Some(ChatMessagePayload::ToolListModulesForUnitContexts(p)) => p,
        _ => {
            return Err("list_modules_for_unit_contexts: missing or wrong payload variant".into());
        }
    };
    let args = parsed.args;
    let tool_call_id = parsed.tool_call_id;
    let tool_name = Some(TOOL_NAME.to_string());
    let context_slugs: Vec<String> = args.context_slugs.clone();
    let rows = block_on_db_tokio(&pool, move |conn| {
        modules_by_context_slugs_visible_tip_units(conn, project_id, session_id, &context_slugs)
    })
    .await
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;
    let by_context: Vec<ContextModulesEntry> = rows
        .into_iter()
        .map(|(context_slug, module_slugs)| ContextModulesEntry {
            context_slug,
            module_slugs,
        })
        .collect();
    let full_payload = ToolListModulesForUnitContextsPayload {
        args,
        by_context,
        tool_call_id,
        tool_name,
    };
    let payload_value = serde_json::to_value(&full_payload)
        .map_err(|e| -> BoxError { format!("serialize list_modules_for_unit_contexts: {}", e).into() })?;
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
