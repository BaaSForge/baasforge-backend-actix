use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerEvent {
    ChatSessionNewMessageChunk(ChatSessionNewMessageChunkPayload),
    ProjectDeleted(ProjectDeletedPayload),
    RuntimeCreated(RuntimeCreatedPayload),
    Subscribed { topic: String },
    Unsubscribed { topic: String },
    Error { message: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectDeletedPayload {
    #[serde(rename = "projectId")]
    pub project_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeCreatedPayload {
    #[serde(rename = "projectId")]
    pub project_id: String,
    #[serde(rename = "runtimeId")]
    pub runtime_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChatSessionNewMessageChunkPayload {
    pub topic: String,
    pub chunk_index: u32,
    pub content: String,
    pub done: bool,
}

impl ChatSessionNewMessageChunkPayload {
    pub fn new(topic: String, chunk_index: u32, content: String, done: bool) -> Self {
        Self {
            topic,
            chunk_index,
            content,
            done,
        }
    }
}
