use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageType, ChatSessionRoleType,
};
use crate::services::message_queue::{
    self, DeadLetterSpec, PublishError, RetryPolicy, SharedRabbitPublisher,
};
use lapin::{Connection, Consumer};
use serde::Serialize;
use serde_json::Value;
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

/// RabbitMQ body for chat worker. `payload` is raw JSON so deserialization uses [`ChatMessageType`]
/// via [`Self::typed_payload`] / [`ChatMessagePayload::from_value`]. Do **not** deserialize
/// [`ChatMessagePayload`] directly from JSON: it is `untagged` and several tool shapes collide
/// (e.g. finish-solution vs finish-implementation; list contexts vs list modules).
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatSessionQueueMessage {
    /// Project owner; required for every queue message (WS channel, `process_one`, tool handlers).
    pub user_id: Uuid,
    pub role: ChatSessionRoleType,
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
    /// UUIDv7 for the AI response message. Worker creates the message with this id when first consumed if not exists.
    pub response_message_id: Uuid,
    /// Retry attempt count. 0 = first attempt; incremented each time the message is sent to the retry delay queue.
    #[serde(default)]
    pub retry_num: u32,
    /// Parent id for the **new** AI `Text` row when consumed (copied to `chat_messages.invoking_message_id`).
    /// Same meaning as on DB rows: `None` = main-thread turn; `Some(outer_tool_id)` = nested under that tool.
    /// After tools finish, continuations copy this from the **completed invoker AI Text row** (its column
    /// `invoking_message_id`), not from the tool row id and not from a “root ancestor” shortcut.
    #[serde(default)]
    pub invoking_message_id: Option<Uuid>,
    /// Agent that will handle this LLM call (used for system prompt and for the AI message row).
    #[serde(default)]
    pub agent: ChatAgentType,
}

impl ChatSessionQueueMessage {
    /// Decode [`Self::payload`] using [`Self::type_`]. Prefer this over deserializing
    /// [`ChatMessagePayload`] from JSON alone.
    pub fn typed_payload(&self) -> Result<Option<ChatMessagePayload>, serde_json::Error> {
        ChatMessagePayload::from_value(&self.type_, self.payload.clone())
    }

    /// Resolves `user_id` and the string passed to [`crate::services::chat_worker::processor::process_one`]
    /// as the current user turn. For `type` [`ChatMessageType::Text`], if `payload` is missing or not
    /// [`ChatMessagePayload::Text`], `user_text` is empty
    /// and the worker still runs the LLM (sub-agents may rely on history only).
    pub fn as_text_message_for_processing(&self) -> Option<(Uuid, String)> {
        if !matches!(self.type_, ChatMessageType::Text) {
            return None;
        }
        let user_id = self.user_id;
        let user_text = match self.typed_payload() {
            Ok(Some(ChatMessagePayload::Text(p))) => p.text.clone(),
            Ok(Some(_)) => {
                tracing::warn!(
                    response_message_id = %self.response_message_id,
                    "chat queue: type is text but payload is not Text; using empty user_text"
                );
                String::new()
            }
            Ok(None) => {
                tracing::warn!(
                    response_message_id = %self.response_message_id,
                    "chat queue: type is text but payload is missing; using empty user_text"
                );
                String::new()
            }
            Err(e) => {
                tracing::warn!(
                    response_message_id = %self.response_message_id,
                    error = %e,
                    "chat queue: failed to decode Text payload; using empty user_text"
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
