use crate::database::DbPool;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageType, ChatSessionRoleType,
};
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::services::chat_worker::llm::tools::{BoxError, ToolSpec};
use crate::services::message_queue::SharedRabbitPublisher;
use gemini_rust::tools::model::FunctionDeclaration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Description for the `intent` parameter (Gemini `JsonSchema` + OpenAI/Claude `params_schema`).
pub const ARG_INTENT_DESCRIPTION: &str = "What you intend to plan: the user’s goals and current architecture analysis.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolAnalyzeSolutionArgs {
    #[schemars(description = ARG_INTENT_DESCRIPTION)]
    pub intent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolAnalyzeSolutionPayload {
    pub args: ToolAnalyzeSolutionArgs,
    pub solution_summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolAnalyzeSolution;

pub const TOOL_NAME: &str = "analyze_solution_tool";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Request an asynchronous solution analysis: propose approaches, trade-offs, and how the plan fits the project’s units and architecture. Describe your intent—requirements, constraints, and what success looks like.",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "intent": {
                "type": "string",
                "description": ARG_INTENT_DESCRIPTION
            }
        },
        "required": ["intent"]
    })
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolAnalyzeSolutionArgs>()
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolAnalyzeSolutionArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolAnalyzeSolution(ToolAnalyzeSolutionPayload {
        args,
        solution_summary: String::new(),
        tool_call_id,
        tool_name: Some(tool_name.to_string()),
    }))
}

/// Queues a new LLM run for [`ChatAgentType::SolutionAnalyzer`] with the intent as the user text.
/// The analyzer must call `finish_solution_analysis` when done, which fills this tool row and completes the planner invoker.
pub async fn handler(
    _pool: DbPool,
    _redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    _payload: Option<Value>,
    rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let publisher = rabbit_publisher.as_ref().ok_or_else(|| -> BoxError {
        "analyze_solution_tool: rabbit publisher missing".into()
    })?;

    let response_message_id = crate::libs::ids::new_uuid();
    let queue_message = ChatSessionQueueMessage {
        user_id: Some(user_id),
        role: ChatSessionRoleType::Ai,
        type_: ChatMessageType::Text,
        payload: None,
        response_message_id,
        retry_num: 0,
        invoking_message_id: Some(message_id),
        agent: ChatAgentType::SolutionAnalyzer,
    };

    chat_message_queue::publish_with_publisher(
        publisher,
        project_id,
        session_id,
        &queue_message,
    )
    .await
    .map_err(|e| -> BoxError { Box::new(e) })?;

    Ok(())
}
