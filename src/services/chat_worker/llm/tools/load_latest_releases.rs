use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::ProjectReleaseResponse;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Description for the `limit` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_LIMIT_DESCRIPTION: &str =
    "Required maximum number of releases to return (20 to max 100).";

/// Args for load_latest_releases. Schema from this struct is used for Gemini; GPT and Claude use params_schema() in spec().
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFetchLatestReleasesArgs {
    #[schemars(description = ARG_LIMIT_DESCRIPTION)]
    pub limit: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolFetchLatestReleasesPayload {
    pub args: ToolFetchLatestReleasesArgs,
    pub releases: Vec<ProjectReleaseResponse>,
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
use crate::services::chat_worker::db::load_latest_releases;
use crate::services::message_queue::SharedRabbitPublisher;
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
pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolFetchLatestReleasesArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolFetchLatestReleases(
        ToolFetchLatestReleasesPayload {
            args,
            releases: vec![],
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
            "limit": {
                "type": "integer",
                "description": ARG_LIMIT_DESCRIPTION
            }
        },
        "required": ["limit"]
    })
}

/// Name of the tool that triggers a RabbitMQ message for the worker to load latest releases for the project.
pub const TOOL_NAME: &str = "load_latest_releases";

/// Single source for GPT and Claude: name, description, and parameters (params_schema).
/// Gemini uses the same name and description but gets parameters from the args struct via `gemini_declaration()`; per-field text is shared via [`ARG_LIMIT_DESCRIPTION`].
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Load the latest releases for the current project. Call this to see release history, versions, and metadata (e.g. before discussing deployments or release versions).",
        parameters: params_schema(),
    }
}

/// Handles a consumed queue message for this tool: load latest releases for the project,
/// update the message with releases and status Completed, then publish Redis status change.
/// Returns Err on any failure; the caller marks the message Failed and runs invoker completion.
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
    let existing = match payload {
        Some(ChatMessagePayload::ToolFetchLatestReleases(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .unwrap_or_default();
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());
    let limit = args.limit;

    let releases = block_on_db_tokio(&pool, move |conn| {
        load_latest_releases(conn, project_id, limit)
    })
    .await
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;
    let full_payload = ToolFetchLatestReleasesPayload {
        args,
        releases,
        tool_call_id,
        tool_name,
        error: None,
    };
    let payload_value = serde_json::to_value(&full_payload)
        .map_err(|e| -> BoxError { format!("serialize releases: {}", e).into() })?;
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
