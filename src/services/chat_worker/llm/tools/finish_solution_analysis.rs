use super::analyze_solution_tool;
use super::{BoxError, ToolSpec, maybe_complete_invoking_message};
use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus,
    ChatMessageType,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::llm::tools::ToolAnalyzeSolutionPayload;
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use gemini_rust::tools::model::FunctionDeclaration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolFinishSolutionAnalysisPayload {
    pub args: ToolFinishSolutionAnalysisArgs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const ARG_DETAILED_REPORT_DESCRIPTION: &str = "Complete solution analysis (plain language, no code): §(1)–(3) unit inventory (units to create, modify, delete—with paths, types, and per-unit specs), §(4) runtime libraries, §(5) sequencing, §(6) risks/trade-offs—not a short recap or vague backing-unit notes. Production-ready scope only unless the user asked for mocks. Write only after investigation is finished.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFinishSolutionAnalysisArgs {
    #[schemars(description = ARG_DETAILED_REPORT_DESCRIPTION)]
    pub detailed_report: String,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolFinishSolutionAnalysis;

pub const TOOL_NAME: &str = "finish_solution_analysis";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Call this when you are done with the solution analysis. Pass detailed_report: the full analysis (not a brief summary) so the planner can continue.",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "detailed_report": {
                "type": "string",
                "description": ARG_DETAILED_REPORT_DESCRIPTION
            }
        },
        "required": ["detailed_report"]
    })
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolFinishSolutionAnalysisArgs>()
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolFinishSolutionAnalysisArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolFinishSolutionAnalysis(
        ToolFinishSolutionAnalysisPayload {
            args,
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

pub async fn handler(
    pool: DbPool,
    redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    payload: Option<ChatMessagePayload>,
    rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let finish_payload: ToolFinishSolutionAnalysisPayload = match payload {
        Some(ChatMessagePayload::ToolFinishSolutionAnalysis(p)) => p,
        _ => {
            return Err("finish_solution_analysis: missing or invalid payload".into());
        }
    };

    let (analyzer_text_id, analyze_tool_id, planner_text_id, analyze_payload) =
        block_on_db_tokio(&pool, move |conn| {
            let finish_row: ChatMessageRow = chat_messages::table
                .filter(chat_messages::id.eq(message_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)?;

            if finish_row.type_ != ChatMessageType::ToolFinishSolutionAnalysis {
                return Err(diesel::result::Error::NotFound);
            }

            let analyzer_text_id = finish_row
                .invoking_message_id
                .ok_or(diesel::result::Error::NotFound)?;

            let analyzer_row: ChatMessageRow = chat_messages::table
                .filter(chat_messages::id.eq(analyzer_text_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)?;

            if analyzer_row.type_ != ChatMessageType::Text
                || analyzer_row.agent.as_ref() != Some(&ChatAgentType::SolutionAnalyzer)
            {
                return Err(diesel::result::Error::NotFound);
            }

            let analyze_tool_id = analyzer_row
                .invoking_message_id
                .ok_or(diesel::result::Error::NotFound)?;

            let analyze_row: ChatMessageRow = chat_messages::table
                .filter(chat_messages::id.eq(analyze_tool_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)?;

            if analyze_row.type_ != ChatMessageType::ToolAnalyzeSolution {
                return Err(diesel::result::Error::NotFound);
            }

            let planner_text_id = analyze_row
                .invoking_message_id
                .ok_or(diesel::result::Error::NotFound)?;

            let analyze_payload = match ChatMessagePayload::from_value(
                &ChatMessageType::ToolAnalyzeSolution,
                analyze_row.payload.clone(),
            ) {
                Ok(Some(ChatMessagePayload::ToolAnalyzeSolution(p))) => p,
                _ => return Err(diesel::result::Error::NotFound),
            };

            Ok::<_, diesel::result::Error>((
                analyzer_text_id,
                analyze_tool_id,
                planner_text_id,
                analyze_payload,
            ))
        })
        .await
        .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;

    let detailed_report = finish_payload.args.detailed_report.clone();

    let merged = ToolAnalyzeSolutionPayload {
        args: analyze_payload.args,
        detailed_report,
        tool_call_id: analyze_payload.tool_call_id,
        tool_name: Some(analyze_solution_tool::TOOL_NAME.to_string()),
        error: None,
    };

    let merged_json = serde_json::to_value(&merged)
        .map_err(|e| -> BoxError { format!("serialize analyze solution payload: {}", e).into() })?;

    let (analyze_updated, finish_updated) = block_on_db_tokio(&pool, move |conn| {
        let analyze_updated =
            diesel::update(chat_messages::table.filter(chat_messages::id.eq(analyze_tool_id)))
                .set((
                    chat_messages::payload.eq(Some(merged_json)),
                    chat_messages::status.eq(ChatMessageStatus::Completed),
                ))
                .returning(ChatMessageRow::as_returning())
                .get_result(conn)?;

        let finish_updated =
            diesel::update(chat_messages::table.filter(chat_messages::id.eq(message_id)))
                .set(chat_messages::status.eq(ChatMessageStatus::Completed))
                .returning(ChatMessageRow::as_returning())
                .get_result(conn)?;

        Ok::<_, diesel::result::Error>((analyze_updated, finish_updated))
    })
    .await
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;

    let channel = user_channel(user_id);
    for row in [analyze_updated, finish_updated] {
        let message = ChatMessageResponse::from(row);
        let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
            project_id,
            session_id,
            message,
        });
        let mut guard = redis_conn.lock().await;
        if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
            tracing::warn!(
                "finish_solution_analysis: failed to publish message update: {}",
                e
            );
        }
    }

    maybe_complete_invoking_message(
        &pool,
        &redis_conn,
        rabbit_publisher.as_ref(),
        analyzer_text_id,
        user_id,
        project_id,
        session_id,
        false,
    )
    .await;

    maybe_complete_invoking_message(
        &pool,
        &redis_conn,
        rabbit_publisher.as_ref(),
        planner_text_id,
        user_id,
        project_id,
        session_id,
        true,
    )
    .await;

    Ok(())
}
