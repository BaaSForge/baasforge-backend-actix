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

pub const ARG_BRIEF_DESCRIPTION: &str = "Short briefing for the observability analyzer: what failed or looks wrong, symptoms, environment (stage vs production deployment), timestamps, trace or log excerpts, and which units or paths you suspect. This is the only handoff from the planner into the nested run—include what matters for diagnosis.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolAnalyzeObservabilityArgs {
    #[schemars(description = ARG_BRIEF_DESCRIPTION)]
    pub brief: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolAnalyzeObservabilityPayload {
    pub args: ToolAnalyzeObservabilityArgs,
    #[serde(default)]
    pub related_units: Vec<ProjectUnitResponse>,
    #[serde(default, alias = "observability_summary")]
    pub detailed_report: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolAnalyzeObservability;

pub const TOOL_NAME: &str = "analyze_observability_tool";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Request an asynchronous observability diagnostics pass: debugging, trace/log interpretation, and likely root causes against a release live unit definitions. Pass **brief** with a compact but concrete summary of the issue (symptoms, excerpts, scope)—the observability analyzer does not see the main chat.",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "brief": {
                "type": "string",
                "description": ARG_BRIEF_DESCRIPTION
            }
        },
        "required": ["brief"]
    })
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolAnalyzeObservabilityArgs>()
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolAnalyzeObservabilityArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolAnalyzeObservability(
        ToolAnalyzeObservabilityPayload {
            args,
            related_units: vec![],
            detailed_report: String::new(),
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

/// Queues a new LLM run for [`ChatAgentType::ObservabilityAnalyzer`] with empty user text; history is nested under this tool row.
/// Does not mark this tool message completed/failed and does not complete the invoking Planner message;
/// the analyzer must call `finish_observability_analysis` when done, which fills this tool row and completes the planner invoker.
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
        "analyze_observability_tool: rabbit publisher missing".into()
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
        agent: ChatAgentType::ObservabilityAnalyzer,
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
