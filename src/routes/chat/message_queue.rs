use crate::models::chat_messages::{ChatMessagePayload, ChatMessageType};
use crate::services::message_queue;
use lapin::Consumer;
use lapin::Connection;
use serde::Serialize;
use std::fmt;

pub const CHAT_SESSION_EXCHANGE: &str = "chat_session_exchange";

pub fn chat_session_routing_key(project_id: i64, session_id: i64) -> String {
    format!("chat_session.{}.{}", project_id, session_id)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatSessionQueueMessage {
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<ChatMessagePayload>,
}

#[derive(Debug)]
pub enum PublishError {
    Serialize(serde_json::Error),
    Queue(lapin::Error),
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PublishError::Serialize(e) => write!(f, "serialize chat message: {}", e),
            PublishError::Queue(e) => write!(f, "message queue: {}", e),
        }
    }
}

impl std::error::Error for PublishError {}

pub async fn publish(
    rabbit_url: &str,
    project_id: i64,
    session_id: i64,
    message: &ChatSessionQueueMessage,
) -> Result<(), PublishError> {
    let payload = serde_json::to_vec(message).map_err(PublishError::Serialize)?;
    message_queue::publish(
        rabbit_url,
        CHAT_SESSION_EXCHANGE,
        &chat_session_routing_key(project_id, session_id),
        &payload,
    )
    .await
    .map_err(PublishError::Queue)
}

pub async fn create_consumer(
    rabbit_url: &str,
    project_id: i64,
    session_id: i64,
) -> Result<(Connection, Consumer), lapin::Error> {
    message_queue::create_consumer(
        rabbit_url,
        CHAT_SESSION_EXCHANGE,
        &chat_session_routing_key(project_id, session_id),
    )
    .await
}
