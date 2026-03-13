use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerEvent {
    ChatSessionNewMessageChunk(ChatSessionNewMessageChunkPayload),
    ProjectDeleted(ProjectDeletedPayload),
    RuntimeCreated(RuntimeCreatedPayload),
    Subscribed { topic: String },
    Unsubscribed { topic: String },
    Error { message: String },
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
    pub message_id: i64,
    pub chunk_index: u32,
    pub content: String,
    pub done: bool,
}

impl ChatSessionNewMessageChunkPayload {
    pub fn new(
        topic: String,
        message_id: i64,
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
