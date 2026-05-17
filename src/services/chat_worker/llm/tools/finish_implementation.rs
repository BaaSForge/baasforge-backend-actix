use super::developer_crud_unit_ids;
use super::implement_solution_tool;
use super::{BoxError, ToolSpec, maybe_complete_invoking_message};
use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::libs::unit_slug::{
    LLM_FULL_UNIT_ID_ITEM_DESCRIPTION, LLM_MUTATED_UNIT_IDS_DESCRIPTION, normalize_full_unit_id_arg,
    validate_full_unit_id_for_tool,
};
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus,
    ChatMessageType,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_related_units_for_chat_session_by_full_ids;
use crate::services::chat_worker::llm::tools::ToolImplementSolutionPayload;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const ARG_DETAILED_REPORT_DESCRIPTION: &str = "Complete implementation report for the planner and user: what was created, modified, or removed (unit identities and behavior), how it satisfies the directive, and any caveats—not a short recap. Write this only after all implementation tool calls are done; omit trial-and-error, exploration steps, and intermediate reasoning.";

pub const ARG_MUTATED_UNIT_IDS_DESCRIPTION: &str = LLM_MUTATED_UNIT_IDS_DESCRIPTION;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFinishImplementationArgs {
    #[schemars(description = ARG_MUTATED_UNIT_IDS_DESCRIPTION)]
    pub mutated_unit_ids: Vec<String>,
    #[schemars(description = ARG_DETAILED_REPORT_DESCRIPTION)]
    pub detailed_report: String,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolFinishImplementation;

pub const TOOL_NAME: &str = "finish_implementation";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Call when implementation is done. Pass mutated_unit_ids as an array of full dot-separated unit id strings for every unit you successfully created, modified, or removed (or [] if none), plus detailed_report: the full account of what was done (not a brief summary) so the planner can continue.",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "mutated_unit_ids": {
                "type": "array",
                "description": ARG_MUTATED_UNIT_IDS_DESCRIPTION,
                "items": {
                    "type": "string",
                    "description": LLM_FULL_UNIT_ID_ITEM_DESCRIPTION
                }
            },
            "detailed_report": {
                "type": "string",
                "description": ARG_DETAILED_REPORT_DESCRIPTION
            }
        },
        "required": ["mutated_unit_ids", "detailed_report"]
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
    let mut args = serde_json::from_value::<ToolFinishImplementationArgs>(args).unwrap_or_default();
    args.mutated_unit_ids = args
        .mutated_unit_ids
        .into_iter()
        .map(|id| normalize_full_unit_id_arg(&id))
        .collect();
    Ok(ChatMessagePayload::ToolFinishImplementation(
        ToolFinishImplementationPayload {
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
    let finish_payload: ToolFinishImplementationPayload = match payload {
        Some(ChatMessagePayload::ToolFinishImplementation(p)) => p,
        _ => {
            return Err("finish_implementation: missing or invalid payload".into());
        }
    };

    let mut full_ids: Vec<String> = Vec::new();
    for id in &finish_payload.args.mutated_unit_ids {
        developer_crud_unit_ids::push_normalized_full_unit_id_unique(&mut full_ids, id);
    }

    let mutated_unit_ids_validation_error: Option<String> = {
        let mut err = None;
        for (i, id) in finish_payload.args.mutated_unit_ids.iter().enumerate() {
            let t = id.trim();
            if t.is_empty() {
                continue;
            }
            if let Err(msg) = validate_full_unit_id_for_tool(t) {
                err = Some(format!(
                    "finish_implementation: mutated_unit_ids[{i}] invalid: {msg}"
                ));
                break;
            }
        }
        err
    };

    let resolve_mutation_units = mutated_unit_ids_validation_error.is_none();

    let (developer_text_id, implement_tool_id, planner_text_id, implement_payload, mutated_units) =
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

            let mutated_units = if resolve_mutation_units {
                load_related_units_for_chat_session_by_full_ids(conn, project_id, session_id, &full_ids)?
            } else {
                vec![]
            };

            Ok::<_, diesel::result::Error>((
                developer_text_id,
                implement_tool_id,
                planner_text_id,
                implement_payload,
                mutated_units,
            ))
        })
        .await
        .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;

    if let Some(err) = mutated_unit_ids_validation_error {
        let finish_out = ToolFinishImplementationPayload {
            args: finish_payload.args.clone(),
            tool_call_id: finish_payload.tool_call_id.clone(),
            tool_name: finish_payload.tool_name.clone(),
            error: Some(err.clone()),
        };
        let merged = ToolImplementSolutionPayload {
            args: implement_payload.args.clone(),
            mutated_units,
            detailed_report: finish_payload.args.detailed_report.clone(),
            tool_call_id: implement_payload.tool_call_id.clone(),
            tool_name: implement_payload.tool_name.clone(),
            error: Some(err),
            incomplete_mutated_units_ids: implement_payload
                .incomplete_mutated_units_ids
                .clone(),
            incomplete_detailed_report_fallback: implement_payload
                .incomplete_detailed_report_fallback
                .clone(),
        };
        let merged_json = serde_json::to_value(&merged).map_err(|e| -> BoxError {
            format!("serialize implement solution payload: {}", e).into()
        })?;
        let finish_json = serde_json::to_value(&finish_out).map_err(|e| -> BoxError {
            format!("serialize finish_implementation payload: {}", e).into()
        })?;

        let (implement_updated, finish_updated) = block_on_db_tokio(&pool, move |conn| {
            let implement_updated =
                diesel::update(chat_messages::table.filter(chat_messages::id.eq(implement_tool_id)))
                    .set((
                        chat_messages::payload.eq(Some(merged_json)),
                        chat_messages::status.eq(ChatMessageStatus::Completed),
                    ))
                    .returning(ChatMessageRow::as_returning())
                    .get_result(conn)?;

            let finish_updated =
                diesel::update(chat_messages::table.filter(chat_messages::id.eq(message_id)))
                    .set((
                        chat_messages::payload.eq(Some(finish_json)),
                        chat_messages::status.eq(ChatMessageStatus::Completed),
                    ))
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

        return Ok(());
    }

    let detailed_report = finish_payload.args.detailed_report.clone();

    let merged = ToolImplementSolutionPayload {
        args: implement_payload.args,
        mutated_units,
        detailed_report,
        tool_call_id: implement_payload.tool_call_id,
        tool_name: Some(implement_solution_tool::TOOL_NAME.to_string()),
        error: None,
        incomplete_mutated_units_ids: vec![],
        incomplete_detailed_report_fallback: None,
    };

    let merged_json = serde_json::to_value(&merged).map_err(|e| -> BoxError {
        format!("serialize implement solution payload: {}", e).into()
    })?;

    let (implement_updated, finish_updated) = block_on_db_tokio(&pool, move |conn| {
        let implement_updated =
            diesel::update(chat_messages::table.filter(chat_messages::id.eq(implement_tool_id)))
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
