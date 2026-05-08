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

/// Planner-facing: one short line—exact units to create or change this run. Do not paste the full solution analysis.
pub const ARG_DIRECTIVE_DESCRIPTION: &str =
    "Exact scope for this run: which units to create or modify and what to implement.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolImplementSolutionArgs {
    #[schemars(description = ARG_DIRECTIVE_DESCRIPTION)]
    pub directive: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolImplementSolutionPayload {
    pub args: ToolImplementSolutionArgs,
    pub detailed_report: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolImplementSolution;

pub const TOOL_NAME: &str = "implement_solution_tool";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Run the programmer agent asynchronously: create or modify units per the agreed solution. Pass the full directive (exact units and changes).",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "directive": {
                "type": "string",
                "description": ARG_DIRECTIVE_DESCRIPTION
            }
        },
        "required": ["directive"]
    })
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolImplementSolutionArgs>()
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolImplementSolutionArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolImplementSolution(
        ToolImplementSolutionPayload {
            args,
            detailed_report: String::new(),
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

/// Queues [`ChatAgentType::Developer`] with empty user text; history is nested under this tool row.
pub async fn handler(
    _pool: DbPool,
    _redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    _payload: Option<ChatMessagePayload>,
    rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let publisher = rabbit_publisher.as_ref().ok_or_else(|| -> BoxError {
        "implement_solution_tool: rabbit publisher missing".into()
    })?;

    let response_message_id = crate::libs::ids::new_uuid();
    let queue_message = ChatSessionQueueMessage {
        user_id,
        role: ChatSessionRoleType::Ai,
        type_: ChatMessageType::Text,
        payload: None,
        response_message_id,
        retry_num: 0,
        invoking_message_id: Some(message_id),
        agent: ChatAgentType::Developer,
        sub_agent_stuck_continuation_count: 0,
        assistant_turn_id: None,
    };

    chat_message_queue::publish_with_publisher(publisher, project_id, session_id, &queue_message)
        .await
        .map_err(|e| -> BoxError { Box::new(e) })?;

    Ok(())
}
