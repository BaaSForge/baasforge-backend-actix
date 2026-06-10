use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::list_distinct_context_slugs_visible_tip_units;
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

/// Args for list_unit_contexts (no parameters; project comes from the session).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolListUnitContextsArgs {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolListUnitContextsPayload {
    #[serde(default)]
    pub args: ToolListUnitContextsArgs,
    /// Distinct bounded-context slugs (`context_slug`) from units visible to this chat session
    /// (base release + session drafts).
    #[serde(default)]
    pub context_slugs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolListUnitContextsArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolListUnitContexts;

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolListUnitContextsArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolListUnitContexts(
        ToolListUnitContextsPayload {
            args,
            context_slugs: vec![],
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

pub const TOOL_NAME: &str = "list_unit_contexts";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "List every distinct bounded-context slug (`context_slug`) from units visible to this chat session — the production release that was live when this chat was created, plus any units created or modified in this session. Call this to see which domains/contexts exist before proposing new units or overlapping names.",
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
    _rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let existing = match payload {
        Some(ChatMessagePayload::ToolListUnitContexts(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .unwrap_or_default();
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());
    let context_slugs = block_on_db_tokio(&pool, move |conn| {
        list_distinct_context_slugs_visible_tip_units(conn, project_id, session_id)
    })
    .await
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;
    let full_payload = ToolListUnitContextsPayload {
        args,
        context_slugs,
        tool_call_id,
        tool_name,
        error: None,
    };
    let payload_value = serde_json::to_value(&full_payload)
        .map_err(|e| -> BoxError { format!("serialize list_unit_contexts: {}", e).into() })?;
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
