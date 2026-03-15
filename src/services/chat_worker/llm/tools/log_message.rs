use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::{ChatMessagePayload, ChatMessageStatus, ChatMessageType};
use crate::routes::ws::events::{ChatSessionMessageStatusChangePayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::schema::chat_messages;
use diesel::prelude::*;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::ToolSpec;
use gemini_rust::tools::model::FunctionDeclaration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ----- Payload types (stored in chat_messages.payload for type_ = ToolLogMessage) -----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolLogMessageArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLogMessageResult {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolLogMessagePayload {
    Args(ToolLogMessageArgs),
    Result(ToolLogMessageResult),
}

// ----- Tool interface -----

/// ChatMessageType for messages produced by this tool. Used to register the handler in the tools module.
pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolLogMessage;

/// Tool name. Part of the standard tool interface.
pub const TOOL_NAME: &str = "log_message";

/// Gemini function declaration with parameter schema.
pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None).with_parameters::<ToolLogMessageArgs>()
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "description": "The message to log"
            }
        },
        "required": ["message"]
    })
}

/// Tool spec for LLM. Part of the standard tool interface.
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Log a message at INFO level. Use for notable user or system events.",
        parameters: params_schema(),
    }
}

/// Builds the queue message payload from LLM tool-call args. Used by [`super::tool_context_for_chat_session`].
pub fn payload_from_args(args: Value) -> Result<ChatMessagePayload, super::BoxError> {
    let args = serde_json::from_value::<ToolLogMessageArgs>(args).unwrap_or_default();
    let payload = ToolLogMessagePayload::Args(args);
    let value = serde_json::to_value(&payload).map_err(|e| -> super::BoxError { Box::new(e) })?;
    Ok(ChatMessagePayload::ToolLogMessage(value))
}

fn message_from_args(payload: &ToolLogMessagePayload) -> String {
    match payload {
        ToolLogMessagePayload::Args(args) => args
            .message
            .as_deref()
            .unwrap_or("(no message)")
            .to_string(),
        ToolLogMessagePayload::Result(r) => r.message.clone(),
    }
}

/// Handles a consumed queue message for this tool: log at INFO, update message with result and status Completed.
pub async fn handler(
    pool: DbPool,
    redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
) {
    let current_payload: Option<ToolLogMessagePayload> = match block_on_db_tokio(&pool, move |conn| {
        chat_messages::table
            .filter(chat_messages::id.eq(message_id))
            .select(chat_messages::payload)
            .get_result::<Option<serde_json::Value>>(conn)
    })
    .await
    {
        Ok(Some(v)) => serde_json::from_value(v).ok(),
        Ok(None) => None,
        Err(e) => {
            tracing::error!(%message_id, "Failed to load log_message message payload: {}", e);
            return;
        }
    };

    let msg = current_payload
        .as_ref()
        .map(message_from_args)
        .unwrap_or_else(|| "(no message)".to_string());

    eprintln!("[tool {}] {}", TOOL_NAME, msg);
    tracing::info!("[tool {}] {}", TOOL_NAME, msg);

    let result_payload = ToolLogMessagePayload::Result(ToolLogMessageResult {
        message: format!("Logged message: {}", msg),
    });
    let payload_value = match serde_json::to_value(&result_payload) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Failed to serialize log_message result: {}", e);
            return;
        }
    };

    if let Err(e) = block_on_db_tokio(&pool, move |conn| {
        diesel::update(
            chat_messages::table.filter(chat_messages::id.eq(message_id)),
        )
        .set((
            chat_messages::payload.eq(Some(payload_value)),
            chat_messages::status.eq(ChatMessageStatus::Completed),
        ))
        .execute(conn)
    })
    .await
    {
        tracing::error!(%message_id, "Failed to update log_message message: {}", e);
        return;
    }

    let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
    let event = WsServerEvent::ChatSessionMessageStatusChange(
        ChatSessionMessageStatusChangePayload {
            message_id,
            status: ChatMessageStatus::Completed,
        },
    );
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &topic, &event).await {
        tracing::warn!("Failed to publish message status change to Redis: {}", e);
    }
}
