use crate::models::chat_messages::{ChatMessageResponse, ChatMessageStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerEvent {
    ChatSessionNewMessageChunk(ChatSessionNewMessageChunkPayload),
    ChatSessionMessageCreated(ChatSessionMessageCreatedPayload),
    ChatSessionMessageStatusChange(ChatSessionMessageStatusChangePayload),
    ProjectDeleted(ProjectDeletedPayload),
    RuntimeCreated(RuntimeCreatedPayload),
    Subscribed { topic: String },
    Unsubscribed { topic: String },
    Error { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatSessionMessageCreatedPayload {
    pub message: ChatMessageResponse,
}

/// Notify clients that a message's status changed (e.g. to `failed` after max retries or `stopped`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatSessionMessageStatusChangePayload {
    #[serde(rename = "messageId")]
    pub message_id: Uuid,
    pub status: ChatMessageStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectDeletedPayload {
    #[serde(rename = "projectId")]
    pub project_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeCreatedPayload {
    #[serde(rename = "projectId")]
    pub project_id: String,
    #[serde(rename = "runtimeId")]
    pub runtime_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatSessionNewMessageChunkPayload {
    pub topic: String,
    /// ID of the placeholder message this chunk belongs to (client appends content to this message).
    pub message_id: Uuid,
    pub chunk_index: u32,
    pub content: String,
    pub done: bool,
}

impl ChatSessionNewMessageChunkPayload {
    pub fn new(
        topic: String,
        message_id: Uuid,
        chunk_index: u32,
        content: String,
        done: bool,
    ) -> Self {
        Self {
            topic,
            message_id,
            chunk_index,
            content,
            done,
        }
    }
}
