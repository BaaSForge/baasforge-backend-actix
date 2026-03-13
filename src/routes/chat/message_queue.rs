use crate::models::chat_messages::{ChatMessagePayload, ChatMessageType, ChatSessionRoleType};
use crate::services::message_queue::{
    self, DeadLetterSpec, PublishError, RetryPolicy, SharedRabbitPublisher,
};
use lapin::{Connection, Consumer};
use serde::Serialize;
use std::io;
use std::sync::Arc;

pub const CHAT_SESSION_EXCHANGE: &str = "chat_session_exchange";
pub const CHAT_WORK_QUEUE: &str = "chat_session_work_queue";
pub const CHAT_WORK_QUEUE_BINDING: &str = "chat_session.#";

/// Retry delay queue: failed messages wait (per-message exponential TTL) then are dead-lettered back to the work exchange.
pub const CHAT_RETRY_DELAY_QUEUE: &str = "retry_delay_chat_session_work_queue";

/// Dead letter queue for chat: messages that exceeded max retries (no TTL; process/log later).
pub const CHAT_DEAD_LETTER_EXCHANGE: &str = "chat_session_dead_letter_exchange";
pub const CHAT_DEAD_LETTER_QUEUE: &str = "chat_session_dead_letter_queue";
pub const CHAT_DEAD_LETTER_BINDING: &str = "chat_session.#";

/// Max retries for chat: we try retry_num 0, 1, 2, 3 (4 attempts); after retry_num 3 fails, send to DLQ.
pub const CHAT_MAX_RETRIES: u32 = 3;

pub fn chat_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: CHAT_MAX_RETRIES,
    }
}

pub fn chat_dead_letter_spec() -> DeadLetterSpec {
    DeadLetterSpec {
        exchange_name: CHAT_DEAD_LETTER_EXCHANGE.to_string(),
        queue_name: CHAT_DEAD_LETTER_QUEUE.to_string(),
        binding_key: CHAT_DEAD_LETTER_BINDING.to_string(),
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
    /// Retry attempt count. 0 = first attempt; incremented each time the message is sent to the retry delay queue.
    #[serde(default)]
    pub retry_num: u32,
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
