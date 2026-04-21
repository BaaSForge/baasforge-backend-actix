use crate::database::DbPool;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageType, ChatSessionRoleType,
};
use crate::models::projects::ProjectUnitResponse;
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
pub const ARG_INTENT_DESCRIPTION: &str = "What you intend to analyze: which units or areas to focus on and what scopes of the project matter for this analysis.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolAnalyzeArchitectureArgs {
    #[schemars(description = ARG_INTENT_DESCRIPTION)]
    pub intent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolAnalyzeArchitecturePayload {
    pub args: ToolAnalyzeArchitectureArgs,
    pub related_units: Vec<ProjectUnitResponse>,
    #[serde(default, alias = "summary")]
    pub detailed_report: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolAnalyzeArchitecture;

pub const TOOL_NAME: &str = "analyze_architecture_tool";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Request an asynchronous architecture analysis for the current project. Describe your intent: what changes or scopes you need to be considered while analyzing.",
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
        .with_parameters::<ToolAnalyzeArchitectureArgs>()
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolAnalyzeArchitectureArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolAnalyzeArchitecture(
        ToolAnalyzeArchitecturePayload {
            args,
            related_units: vec![],
            detailed_report: String::new(),
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

/// Queues a new LLM run for [`ChatAgentType::ArchitectureAnalyzer`] with the intent as the user text.
/// Does not mark this tool message completed/failed and does not complete the invoking Planner message;
/// the analyzer must call `finish_architecture_analysis` when done, which fills this tool row and completes the planner invoker.
/// See [`super::tool_message_type_skips_generic_invoker_completion`]: on success the generic invoker completion path is skipped.
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
        "analyze_architecture_tool: rabbit publisher missing".into()
    })?;

    let response_message_id = crate::libs::ids::new_uuid();
    // `invoking_message_id` = this tool row: sub-agent Text is nested under the tool; nested tools
    // under that agent use `tool_context` → parent = that Text message id (see `process_one`).
    let queue_message = ChatSessionQueueMessage {
        user_id,
        role: ChatSessionRoleType::Ai,
        type_: ChatMessageType::Text,
        payload: None,
        response_message_id,
        retry_num: 0,
        invoking_message_id: Some(message_id),
        agent: ChatAgentType::ArchitectureAnalyzer,
        sub_agent_stuck_continuation_count: 0,
        assistant_turn_id: None,
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
