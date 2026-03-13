use crate::models::chat_messages::{ChatMessagePayload, ChatMessageType, ChatSessionRoleType};
use crate::services::message_queue::{self, PublishError, RetryDelaySpec, SharedRabbitPublisher};
use lapin::{Connection, Consumer};
use serde::Serialize;
use std::io;
use std::sync::Arc;

pub const CHAT_SESSION_EXCHANGE: &str = "chat_session_exchange";
pub const CHAT_WORK_QUEUE: &str = "chat_session_work_queue";
pub const CHAT_WORK_QUEUE_BINDING: &str = "chat_session.#";

/// Delay queue for chat work: failed messages go here, wait TTL (e.g. 3s), then are dead-lettered back to the chat work queue (original routing key preserved).
pub const CHAT_RETRY_DELAY_QUEUE: &str = "delay_3s_chat_session_work_queue";

pub fn chat_retry_delay_spec() -> RetryDelaySpec {
    RetryDelaySpec {
        delay_queue_name: CHAT_RETRY_DELAY_QUEUE.to_string(),
        retry_binding_key: CHAT_WORK_QUEUE_BINDING.to_string(),
        dead_letter_exchange: CHAT_SESSION_EXCHANGE.to_string(),
        dead_letter_routing_key: None,
    }
}

pub fn chat_session_routing_key(project_id: i64, session_id: i64) -> String {
    format!("chat_session.{}.{}", project_id, session_id)
}

pub fn parse_chat_routing_key(routing_key: &str) -> Option<(i64, i64)> {
    let rest = routing_key.strip_prefix("chat_session.")?;
    let mut parts = rest.split('.');
    let project_id: i64 = parts.next()?.parse().ok()?;
    let session_id: i64 = parts.next()?.parse().ok()?;
    Some((project_id, session_id))
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatSessionQueueMessage {
    #[serde(default)]
    pub user_id: Option<i64>,
    pub role: ChatSessionRoleType,
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<ChatMessagePayload>,
    /// ID of the placeholder AI message row (status=generating). Required; message is not processed without it.
    pub placeholder_ai_message_id: i64,
}

impl ChatSessionQueueMessage {
    pub fn validate(&self) -> Result<(), &'static str> {
        if matches!(self.role, ChatSessionRoleType::User) && self.user_id.is_none() {
            return Err("user_id is required when role is User");
        }
        Ok(())
    }

    // TODO: We may need to remove this logic and handle it inside ChatMessagePayload
    pub fn as_user_text_message(&self) -> Option<(i64, String)> {
        if !matches!(self.role, ChatSessionRoleType::User) {
            return None;
        }
        let user_id = self.user_id?;
        if !matches!(self.type_, ChatMessageType::Text) {
            return None;
        }
        let text = match &self.payload {
            Some(ChatMessagePayload::Text(s)) => s.clone(),
            _ => return None,
        };
        Some((user_id, text))
    }
}

pub async fn publish_with_publisher(
    publisher: &Arc<SharedRabbitPublisher>,
    project_id: i64,
    session_id: i64,
    message: &ChatSessionQueueMessage,
) -> Result<(), PublishError> {
    message
        .validate()
        .map_err(|e| PublishError::Serialize(serde_json::Error::io(io::Error::new(io::ErrorKind::InvalidData, e))))?;
    let payload = serde_json::to_vec(message).map_err(PublishError::Serialize)?;
    message_queue::publish_with_channel(
        publisher.as_ref(),
        CHAT_SESSION_EXCHANGE,
        &chat_session_routing_key(project_id, session_id),
        &payload,
    )
    .await
    .map_err(PublishError::Queue)
}

pub async fn create_chat_work_consumer_on_connection(
    connection: &Connection,
) -> Result<(lapin::Channel, Consumer), lapin::Error> {
    message_queue::create_work_consumer_on_connection(
        connection,
        CHAT_SESSION_EXCHANGE,
        CHAT_WORK_QUEUE,
        CHAT_WORK_QUEUE_BINDING,
    )
    .await
}
