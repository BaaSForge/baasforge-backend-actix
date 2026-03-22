use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageType, ChatSessionRoleType,
};
use crate::services::message_queue::{
    self, DeadLetterSpec, PublishError, RetryPolicy, SharedRabbitPublisher,
};
use lapin::{Connection, Consumer};
use serde::Serialize;
use std::io;
use std::sync::Arc;
use uuid::Uuid;

pub const CHAT_SESSION_EXCHANGE: &str = "chat_session_exchange";
pub const CHAT_WORK_QUEUE: &str = "chat_session_work_queue";
pub const CHAT_WORK_QUEUE_BINDING: &str = "chat_session.#";

/// Retry delay queue: failed messages wait (per-message exponential TTL) then are dead-lettered back to the work exchange.
pub const CHAT_RETRY_DELAY_QUEUE: &str = "retry_delay_chat_session_work_queue";

/// Dead letter queue for chat: messages that exceeded max retries (no TTL; process/log later).
pub const CHAT_DEAD_LETTER_EXCHANGE: &str = "chat_session_dead_letter_exchange";
pub const CHAT_DEAD_LETTER_QUEUE: &str = "chat_session_dead_letter_queue";
pub const CHAT_DEAD_LETTER_BINDING: &str = "chat_session.#";

/// Max retries for chat: we try retry_num 0, 1 (2 attempts); after retry_num 1 fails, send to DLQ.
pub const CHAT_MAX_RETRIES: u32 = 1;

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

pub fn chat_session_routing_key(project_id: Uuid, session_id: Uuid) -> String {
    format!("chat_session.{}.{}", project_id, session_id)
}

pub fn parse_chat_routing_key(routing_key: &str) -> Option<(Uuid, Uuid)> {
    let rest = routing_key.strip_prefix("chat_session.")?;
    let mut parts = rest.split('.');
    let project_id = parts.next().and_then(|s| Uuid::parse_str(s).ok())?;
    let session_id = parts.next().and_then(|s| Uuid::parse_str(s).ok())?;
    Some((project_id, session_id))
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatSessionQueueMessage {
    #[serde(default)]
    pub user_id: Option<Uuid>,
    pub role: ChatSessionRoleType,
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<ChatMessagePayload>,
    /// UUIDv7 for the AI response message. Worker creates the message with this id when first consumed if not exists.
    pub response_message_id: Uuid,
    /// Retry attempt count. 0 = first attempt; incremented each time the message is sent to the retry delay queue.
    #[serde(default)]
    pub retry_num: u32,
    /// Parent id for this row when consumed from the queue (copied to `chat_messages.invoking_message_id`).
    /// See [`crate::models::chat_messages::ChatMessageRow`] for the full hierarchy (tool rows → invoker
    /// AI message; sub-agent `Text` → outer tool row; nested tools → sub-agent AI message).
    #[serde(default)]
    pub invoking_message_id: Option<Uuid>,
    /// Agent that will handle this LLM call (used for system prompt and for the AI message row).
    #[serde(default)]
    pub agent: ChatAgentType,
}

impl ChatSessionQueueMessage {
    pub fn validate(&self) -> Result<(), &'static str> {
        if matches!(self.role, ChatSessionRoleType::User) && self.user_id.is_none() {
            return Err("user_id is required when role is User");
        }
        // Ai + Text is used for continuation after tool calls; user_id is required to run process_one.
        if matches!(self.role, ChatSessionRoleType::Ai)
            && matches!(self.type_, ChatMessageType::Text)
            && self.user_id.is_none()
        {
            return Err("user_id is required when role is Ai and type is Text");
        }
        Ok(())
    }

    /// Resolves `user_id` and the string passed to [`crate::services::chat_worker::processor::process_one`]
    /// as the current user turn. For `type` [`ChatMessageType::Text`], [`Self::validate`] must have ensured
    /// `user_id` is set. If `payload` is missing or not [`ChatMessagePayload::Text`], `user_text` is empty
    /// and the worker still runs the LLM (sub-agents may rely on history only).
    pub fn as_text_message_for_processing(&self) -> Option<(Uuid, String)> {
        if !matches!(self.type_, ChatMessageType::Text) {
            return None;
        }
        let user_id = self.user_id?;
        let user_text = match &self.payload {
            Some(ChatMessagePayload::Text(p)) => p.text.clone(),
            Some(_) => {
                tracing::warn!(
                    response_message_id = %self.response_message_id,
                    "chat queue: type is text but payload is not Text; using empty user_text"
                );
                String::new()
            }
            None => {
                tracing::warn!(
                    response_message_id = %self.response_message_id,
                    "chat queue: type is text but payload is missing; using empty user_text"
                );
                String::new()
            }
        };
        Some((user_id, user_text))
    }
}

pub async fn publish_with_publisher(
    publisher: &Arc<SharedRabbitPublisher>,
    project_id: Uuid,
    session_id: Uuid,
    message: &ChatSessionQueueMessage,
) -> Result<(), PublishError> {
    message.validate().map_err(|e| {
        PublishError::Serialize(serde_json::Error::io(io::Error::new(
            io::ErrorKind::InvalidData,
            e,
        )))
    })?;
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
