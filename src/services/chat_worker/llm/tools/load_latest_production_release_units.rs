use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::ProjectUnitResponse;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFetchRelevantUnitsArgs {
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolFetchRelevantUnitsPayload {
    pub args: ToolFetchRelevantUnitsArgs,
    pub units: Vec<ProjectUnitResponse>,
}
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_units_for_latest_production_release;
use diesel::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{BoxError, ToolSpec};
use gemini_rust::tools::model::FunctionDeclaration;

/// Gemini function declaration with parameter schema so the model receives required arguments.
pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolFetchRelevantUnitsArgs>()
}

/// ChatMessageType for messages produced by this tool. Used to register the handler in the tools module.
pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolFetchRelevantUnits;

/// Builds the queue message payload from LLM tool-call args. Used by [`super::tool_context_for_chat_session`].
pub fn payload_from_args(args: Value) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolFetchRelevantUnitsArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolFetchRelevantUnits(
        ToolFetchRelevantUnitsPayload {
            args,
            units: vec![],
        },
    ))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Search query to filter or find relevant units in the latest production release (e.g. user's question, keywords, or a short phrase). Must not be empty."
            }
        },
        "required": ["query"]
    })
}

/// Name of the tool that triggers a RabbitMQ message for the worker to load latest production release units (not executed inline).
pub const TOOL_NAME: &str = "load_latest_production_release_units";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Load units from the latest production release for the current project, filtered by a search query. You MUST always pass the 'query' parameter: set it to the user's exact question or a short phrase summarizing their intent (e.g. 'auth controller', 'payment flow'). Never call this with an empty or missing query. Call this to understand the current production architecture before creating or modifying units.",
        parameters: params_schema(),
    }
}

/// Handles a consumed queue message for this tool: load units for the latest production release,
/// update the message with units and status Completed, then publish Redis status change.
/// Returns Err on any failure; the caller marks the message Failed and runs invoker completion.
pub async fn handler(
    pool: DbPool,
    redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    payload: Option<Value>,
) -> Result<(), BoxError> {
    let existing = payload
        .as_ref()
        .and_then(|v| serde_json::from_value::<ToolFetchRelevantUnitsPayload>(v.clone()).ok());
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .unwrap_or_default();
    let query_owned = args.query.clone();
    let units = block_on_db_tokio(&pool, move |conn| {
        load_units_for_latest_production_release(conn, project_id, &query_owned)
    })
    .await
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;
    let full_payload = ToolFetchRelevantUnitsPayload { args, units };
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
    let mut guard = redis_conn.lock().await;
    publish_event_to_channel(&mut *guard, &channel, &event)
        .await
        .map_err(|e| -> BoxError { e.to_string().into() })?;
    Ok(())
}
