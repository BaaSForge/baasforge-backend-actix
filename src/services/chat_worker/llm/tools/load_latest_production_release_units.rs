use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::ProjectUnitResponse;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Description for the `query` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_QUERY_DESCRIPTION: &str = "Required. Semantic search query to find relevant units visible to this chat session — the production release that was live when this chat was created, plus any units created or modified in this session. Pass a natural-language sentence or keywords describing intent (e.g. 'authentication and authorization logic', 'unit and module for inventory'). Must not be empty. Must not be '*' alone — large projects cannot return unbounded unit sets; always supply a substantive description of what to retrieve (even when the user wants a broad overview).";

/// Args for load_latest_production_release_units. Schema from this struct is used for Gemini; GPT and Claude use params_schema() in spec().
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFetchRelevantUnitsArgs {
    #[schemars(description = ARG_QUERY_DESCRIPTION)]
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolFetchRelevantUnitsPayload {
    pub args: ToolFetchRelevantUnitsArgs,
    pub units: Vec<ProjectUnitResponse>,
    /// LLM tool call id; used when building message history so tool results attach to the correct tool_use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool name (e.g. TOOL_NAME); stored so upstream LLM history can use it without looking up from a previous message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_units_for_latest_production_release;
use crate::services::message_queue::SharedRabbitPublisher;
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
pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let mut args = serde_json::from_value::<ToolFetchRelevantUnitsArgs>(args).unwrap_or_default();
    args.query = args.query.trim().to_string();
    Ok(ChatMessagePayload::ToolFetchRelevantUnits(
        ToolFetchRelevantUnitsPayload {
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
            "query": {
                "type": "string",
                "description": ARG_QUERY_DESCRIPTION
            }
        },
        "required": ["query"]
    })
}

/// Name of the tool that triggers a RabbitMQ message for the worker to load latest production release units (not executed inline).
pub const TOOL_NAME: &str = "load_latest_production_release_units";

/// Single source for GPT and Claude: name, description, and parameters (params_schema).
/// Gemini uses the same name and description but gets parameters from the args struct via `gemini_declaration()`; per-field text is shared via [`ARG_QUERY_DESCRIPTION`].
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Search units visible to this chat session — the production release that was live when this chat was created, plus any units created or modified in this session. The query is matched semantically against unit content (required). You MUST pass a non-empty `query` every time: describe what you need in natural language (e.g. 'authentication and authorization logic', 'everything related to billing'). Do not omit `query`, do not pass an empty string, and do not pass '*' alone — large projects cannot afford unbounded listing without a stated intent. If the user wants a broad overview, call this tool multiple times with different queries. Use this to discover existing units before proposing creates or edits.",
        parameters: params_schema(),
    }
}

/// Handles a consumed queue message for this tool: load units for the latest production release,
/// update the message with units and status Completed, then publish Redis status change.
/// Returns Err on any failure; the caller marks the message Failed and runs invoker completion.
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
        Some(ChatMessagePayload::ToolFetchRelevantUnits(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .unwrap_or_default();
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());
    let query_owned = args.query.clone();
    let query_trimmed = query_owned.trim();
    let query_invalid = query_trimmed.is_empty() || query_trimmed == "*";

    let (units, error) = if query_invalid {
        (
            vec![],
            Some(
                "load_latest_production_release_units: `query` is required and must be non-empty; must not be '*' alone. Pass a natural-language description of what units to find (e.g. the user's question or topic)."
                    .to_string(),
            ),
        )
    } else {
        let units = block_on_db_tokio(&pool, move |conn| {
            load_units_for_latest_production_release(conn, project_id, session_id, &query_owned)
        })
        .await
        .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;
        (units, None)
    };

    let full_payload = ToolFetchRelevantUnitsPayload {
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
