use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::{ChatMessagePayload, ChatMessageStatus, ChatMessageType};
use crate::models::projects::ProjectUnitResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ToolFetchRelevantUnitsArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolFetchRelevantUnitsPayload {
    Args(ToolFetchRelevantUnitsArgs),
    Units(Vec<ProjectUnitResponse>),
}
use crate::routes::ws::events::{ChatSessionMessageStatusChangePayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_units_for_latest_production_release;
use diesel::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{BoxError, ToolSpec};

/// ChatMessageType for messages produced by this tool. Used to register the handler in the tools module.
pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolFetchRelevantUnits;

/// Builds the queue message payload from LLM tool-call args. Used by [`super::tool_context_for_chat_session`].
pub fn payload_from_args(args: Value) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolFetchRelevantUnitsArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolFetchRelevantUnits(
        ToolFetchRelevantUnitsPayload::Args(args),
    ))
}

/// Schema for load_latest_production_release_units: optional args the LLM can pass.
fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {}
    })
}

/// Name of the tool that triggers a RabbitMQ message for the worker to load latest production release units (not executed inline).
pub const TOOL_NAME: &str = "load_latest_production_release_units";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Load units from the latest production release for the current project. Call this to understand the current production architecture before creating or modifying units.",
        parameters: params_schema(),
    }
}

/// Handles a consumed queue message for this tool: load units for the latest production release,
/// update the message with units and status Completed, then publish Redis status change.
pub async fn handler(
    pool: DbPool,
    redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
) {
    let units = match block_on_db_tokio(&pool, move |conn| {
        load_units_for_latest_production_release(conn, project_id)
    })
    .await
    {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(
                %project_id,
                %session_id,
                %message_id,
                "Failed to load latest production release units: {}",
                e
            );
            let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
            let event = WsServerEvent::ChatSessionMessageStatusChange(
                ChatSessionMessageStatusChangePayload {
                    message_id,
                    status: ChatMessageStatus::Failed,
                },
            );
            let mut guard = redis_conn.lock().await;
            let _ = publish_event_to_channel(&mut *guard, &topic, &event).await;
            return;
        }
    };
    let payload_value = match serde_json::to_value(&units) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Failed to serialize units for message: {}", e);
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
        tracing::error!(
            %message_id,
            "Failed to update load_latest_production_release_units message: {}",
            e
        );
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
