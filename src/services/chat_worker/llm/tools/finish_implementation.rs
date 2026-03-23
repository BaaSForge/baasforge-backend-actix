use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus,
    ChatMessageType,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::llm::tools::ToolImplementSolutionPayload;
use super::implement_solution_tool;
use super::{maybe_complete_invoking_message, BoxError, ToolSpec};
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
pub struct ToolFinishImplementationPayload {
    pub args: ToolFinishImplementationArgs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

pub const ARG_IMPLEMENTATION_SUMMARY_DESCRIPTION: &str =
    "Brief summary for the planner: what units were created or changed and the outcome.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFinishImplementationArgs {
    #[schemars(description = ARG_IMPLEMENTATION_SUMMARY_DESCRIPTION)]
    pub implementation_summary: String,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolFinishImplementation;

pub const TOOL_NAME: &str = "finish_implementation";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Call when implementation is done. Pass a concise summary so the planner can continue.",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "implementation_summary": {
                "type": "string",
                "description": ARG_IMPLEMENTATION_SUMMARY_DESCRIPTION
            }
        },
        "required": ["implementation_summary"]
    })
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolFinishImplementationArgs>()
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolFinishImplementationArgs>(args).unwrap_or_default();
    Ok(ChatMessagePayload::ToolFinishImplementation(
        ToolFinishImplementationPayload {
            args,
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
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
    payload: Option<Value>,
    rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let finish_payload: ToolFinishImplementationPayload = payload
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .ok_or_else(|| -> BoxError {
            "finish_implementation: missing or invalid payload".into()
        })?;

    let (developer_text_id, implement_tool_id, planner_text_id, implement_payload) =
        block_on_db_tokio(&pool, move |conn| {
            let finish_row: ChatMessageRow = chat_messages::table
                .filter(chat_messages::id.eq(message_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)?;

            if finish_row.type_ != ChatMessageType::ToolFinishImplementation {
                return Err(diesel::result::Error::NotFound);
            }

            let developer_text_id = finish_row
                .invoking_message_id
                .ok_or(diesel::result::Error::NotFound)?;

            let developer_row: ChatMessageRow = chat_messages::table
                .filter(chat_messages::id.eq(developer_text_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)?;

            if developer_row.type_ != ChatMessageType::Text
                || developer_row.agent.as_ref() != Some(&ChatAgentType::Developer)
            {
                return Err(diesel::result::Error::NotFound);
            }

            let implement_tool_id = developer_row
                .invoking_message_id
                .ok_or(diesel::result::Error::NotFound)?;

            let implement_row: ChatMessageRow = chat_messages::table
                .filter(chat_messages::id.eq(implement_tool_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)?;

            if implement_row.type_ != ChatMessageType::ToolImplementSolution {
                return Err(diesel::result::Error::NotFound);
            }

            let planner_text_id = implement_row
                .invoking_message_id
                .ok_or(diesel::result::Error::NotFound)?;

            let implement_payload = match ChatMessagePayload::from_value(
                &ChatMessageType::ToolImplementSolution,
                implement_row.payload.clone(),
            ) {
                Ok(Some(ChatMessagePayload::ToolImplementSolution(p))) => p,
                _ => return Err(diesel::result::Error::NotFound),
            };

            Ok::<_, diesel::result::Error>((
                developer_text_id,
                implement_tool_id,
                planner_text_id,
                implement_payload,
            ))
        })
        .await
        .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;

    let implementation_summary = finish_payload.args.implementation_summary.clone();

    let merged = ToolImplementSolutionPayload {
        args: implement_payload.args,
        implementation_summary,
        tool_call_id: implement_payload.tool_call_id,
        tool_name: Some(implement_solution_tool::TOOL_NAME.to_string()),
    };

    let merged_json = serde_json::to_value(&merged)
        .map_err(|e| -> BoxError { format!("serialize implement solution payload: {}", e).into() })?;

    let (implement_updated, finish_updated) = block_on_db_tokio(&pool, move |conn| {
        let implement_updated = diesel::update(
            chat_messages::table.filter(chat_messages::id.eq(implement_tool_id)),
        )
        .set((
            chat_messages::payload.eq(Some(merged_json)),
            chat_messages::status.eq(ChatMessageStatus::Completed),
        ))
        .returning(ChatMessageRow::as_returning())
        .get_result(conn)?;

        let finish_updated = diesel::update(chat_messages::table.filter(chat_messages::id.eq(message_id)))
            .set(chat_messages::status.eq(ChatMessageStatus::Completed))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)?;

        Ok::<_, diesel::result::Error>((implement_updated, finish_updated))
    })
    .await
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;

    let channel = user_channel(user_id);
    for row in [implement_updated, finish_updated] {
        let message = ChatMessageResponse::from(row);
        let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
            project_id,
            session_id,
            message,
        });
        let mut guard = redis_conn.lock().await;
        if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
            tracing::warn!(
                "finish_implementation: failed to publish message update: {}",
                e
            );
        }
    }

    maybe_complete_invoking_message(
        &pool,
        &redis_conn,
        rabbit_publisher.as_ref(),
        developer_text_id,
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
