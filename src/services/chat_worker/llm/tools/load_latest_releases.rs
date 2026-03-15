use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::{ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType};
use crate::models::projects::ProjectReleaseResponse;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFetchLatestReleasesArgs {
    pub limit: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolFetchLatestReleasesPayload {
    pub args: ToolFetchLatestReleasesArgs,
    pub releases: Vec<ProjectReleaseResponse>,
}

use crate::routes::ws::events::{ChatSessionMessageStatusChangePayload, ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_latest_releases;
use diesel::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{BoxError, ToolSpec};
use gemini_rust::tools::model::FunctionDeclaration;

/// Gemini function declaration with parameter schema.
pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolFetchLatestReleasesArgs>()
}

/// ChatMessageType for messages produced by this tool. Used to register the handler in the tools module.
pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolFetchLatestReleases;

/// Builds the queue message payload from LLM tool-call args. Used by [`super::tool_context_for_chat_session`].
pub fn payload_from_args(args: Value) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolFetchLatestReleasesArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolFetchLatestReleases(
        ToolFetchLatestReleasesPayload {
            args,
            releases: vec![],
        },
    ))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "limit": {
                "type": "integer",
                "description": "Required maximum number of releases to return (20 to max 100)."
            }
        }
    })
}

/// Name of the tool that triggers a RabbitMQ message for the worker to load latest releases for the project.
pub const TOOL_NAME: &str = "load_latest_releases";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Load the latest releases for the current project. Call this to see release history, versions, and metadata (e.g. before discussing deployments or release versions).",
        parameters: params_schema(),
    }
}

/// Handles a consumed queue message for this tool: load latest releases for the project,
/// update the message with releases and status Completed, then publish Redis status change.
pub async fn handler(
    pool: DbPool,
    redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    payload: Option<Value>,
) {
    let existing = payload
        .as_ref()
        .and_then(|v| serde_json::from_value::<ToolFetchLatestReleasesPayload>(v.clone()).ok());
    let args = existing.as_ref().map(|p| p.args.clone()).unwrap_or_default();
    let limit = args.limit;

    let releases = match block_on_db_tokio(&pool, move |conn| {
        load_latest_releases(conn, project_id, limit)
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                %project_id,
                %session_id,
                %message_id,
                "Failed to load latest releases: {}",
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
    let full_payload = ToolFetchLatestReleasesPayload {
        args,
        releases,
    };
    let payload_value = match serde_json::to_value(&full_payload) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Failed to serialize releases for message: {}", e);
            return;
        }
    };
    let updated_row = match block_on_db_tokio(&pool, move |conn| {
        diesel::update(
            chat_messages::table.filter(chat_messages::id.eq(message_id)),
        )
        .set((
            chat_messages::payload.eq(Some(payload_value)),
            chat_messages::status.eq(ChatMessageStatus::Completed),
        ))
        .returning(ChatMessageRow::as_returning())
        .get_result(conn)
    })
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(
                %message_id,
                "Failed to update load_latest_releases message: {}",
                e
            );
            return;
        }
    };
    let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
    let message: ChatMessageResponse = updated_row.into();
    let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
        message,
    });
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &topic, &event).await {
        tracing::warn!("Failed to publish message updated to Redis: {}", e);
    }
}
