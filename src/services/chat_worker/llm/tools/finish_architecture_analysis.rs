use super::analyze_architecture_tool;
use super::{BoxError, ToolAnalyzeArchitecturePayload, ToolSpec, maybe_complete_invoking_message};
use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::libs::unit_slug::{
    LLM_FULL_UNIT_ID_ITEM_DESCRIPTION, LLM_RELEVANT_UNIT_IDS_DESCRIPTION,
    normalize_full_unit_id_arg, validate_full_unit_id_for_tool,
};
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus,
    ChatMessageType,
};
use crate::models::projects::ProjectUnitResponse;
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_related_units_for_chat_session_by_full_ids;
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
pub struct ToolFinishArchitectureAnalysisPayload {
    pub args: ToolFinishArchitectureAnalysisArgs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub const ARG_RELEVANT_UNIT_IDS_DESCRIPTION: &str = LLM_RELEVANT_UNIT_IDS_DESCRIPTION;

pub const ARG_DETAILED_REPORT_DESCRIPTION: &str = "Complete architecture analysis for the planner and user: full explanation of current units, relationships, gaps, and risks—not a short recap. Write this only after all tool-based investigation is finished; omit trial-and-error, exploration steps, and intermediate reasoning.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFinishArchitectureAnalysisArgs {
    #[schemars(description = ARG_RELEVANT_UNIT_IDS_DESCRIPTION)]
    pub relevant_unit_ids: Vec<String>,
    #[schemars(description = ARG_DETAILED_REPORT_DESCRIPTION)]
    pub detailed_report: String,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolFinishArchitectureAnalysis;

pub const TOOL_NAME: &str = "finish_architecture_analysis";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Call this when you are done with the architecture analysis. Pass relevant_unit_ids as an array of full dot-separated unit id strings (or [] if none), plus detailed_report: the full analysis (not a brief summary).",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "relevant_unit_ids": {
                "type": "array",
                "description": ARG_RELEVANT_UNIT_IDS_DESCRIPTION,
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
        "required": ["relevant_unit_ids", "detailed_report"]
    })
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolFinishArchitectureAnalysisArgs>()
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let mut args =
        serde_json::from_value::<ToolFinishArchitectureAnalysisArgs>(args).unwrap_or_default();
    args.relevant_unit_ids = args
        .relevant_unit_ids
        .into_iter()
        .map(|id| normalize_full_unit_id_arg(&id))
        .collect();
    Ok(ChatMessagePayload::ToolFinishArchitectureAnalysis(
        ToolFinishArchitectureAnalysisPayload {
            args,
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

/// Merges detailed_report and units into the outer `ToolAnalyzeArchitecture` row, marks this tool and that
/// row completed, completes the nested analyzer `Text` without queueing another analyzer pass, then
/// runs [`maybe_complete_invoking_message`] on the planner invoker (same effect as
/// [`super::on_tool_message_finished`] for the outer tool).
/// `related_units` are resolved from the session effective unit set (baseline release + draft tips), same as **load_latest_production_release_units**.
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
    let finish_payload: ToolFinishArchitectureAnalysisPayload = match payload {
        Some(ChatMessagePayload::ToolFinishArchitectureAnalysis(p)) => p,
        _ => {
            return Err("finish_architecture_analysis: missing or invalid payload".into());
        }
    };

    let (analyzer_text_id, analyze_tool_id, planner_text_id, analyze_payload) =
        block_on_db_tokio(&pool, move |conn| {
            let finish_row: ChatMessageRow = chat_messages::table
                .filter(chat_messages::id.eq(message_id))
                .filter(chat_messages::chat_session_id.eq(session_id))
                .select(ChatMessageRow::as_select())
                .first(conn)?;

            if finish_row.type_ != ChatMessageType::ToolFinishArchitectureAnalysis {
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
                || analyzer_row.agent.as_ref() != Some(&ChatAgentType::ArchitectureAnalyzer)
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

            if analyze_row.type_ != ChatMessageType::ToolAnalyzeArchitecture {
                return Err(diesel::result::Error::NotFound);
            }

            let planner_text_id = analyze_row
                .invoking_message_id
                .ok_or(diesel::result::Error::NotFound)?;

            let analyze_payload = match ChatMessagePayload::from_value(
                &ChatMessageType::ToolAnalyzeArchitecture,
                analyze_row.payload.clone(),
            ) {
                Ok(Some(ChatMessagePayload::ToolAnalyzeArchitecture(p))) => p,
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

    let relevant_unit_ids_validation_error: Option<String> = {
        let mut err = None;
        for (i, id) in finish_payload.args.relevant_unit_ids.iter().enumerate() {
            let t = id.trim();
            if t.is_empty() {
                continue;
            }
            if let Err(msg) = validate_full_unit_id_for_tool(t) {
                err = Some(format!(
                    "finish_architecture_analysis: relevant_unit_ids[{i}] invalid: {msg}"
                ));
                break;
            }
        }
        err
    };

    if let Some(err) = relevant_unit_ids_validation_error {
        let finish_out = ToolFinishArchitectureAnalysisPayload {
            args: finish_payload.args.clone(),
            tool_call_id: finish_payload.tool_call_id.clone(),
            tool_name: finish_payload.tool_name.clone(),
            error: Some(err.clone()),
        };
        let merged = ToolAnalyzeArchitecturePayload {
            args: analyze_payload.args.clone(),
            related_units: vec![],
            detailed_report: finish_payload.args.detailed_report.clone(),
            tool_call_id: analyze_payload.tool_call_id.clone(),
            tool_name: analyze_payload.tool_name.clone(),
            error: Some(err),
        };
        let merged_json = serde_json::to_value(&merged)
            .map_err(|e| -> BoxError { format!("serialize analyze payload: {}", e).into() })?;
        let finish_json = serde_json::to_value(&finish_out).map_err(|e| -> BoxError {
            format!("serialize finish_architecture payload: {}", e).into()
        })?;

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
                    .set((
                        chat_messages::payload.eq(Some(finish_json)),
                        chat_messages::status.eq(ChatMessageStatus::Completed),
                    ))
                    .returning(ChatMessageRow::as_returning())
                    .get_result(conn)?;

            Ok::<_, diesel::result::Error>((analyze_updated, finish_updated))
        })
        .await
        .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;

        let channel = user_channel(user_id);
        for row in [analyze_updated, finish_updated] {
            let message = ChatMessageResponse::from(row);
            let event =
                WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
                    project_id,
                    session_id,
                    message,
                });
            let mut guard = redis_conn.lock().await;
            if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
                tracing::warn!(
                    "finish_architecture_analysis: failed to publish message update: {}",
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

        return Ok(());
    }

    let full_ids: Vec<String> = finish_payload
        .args
        .relevant_unit_ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    let detailed_report = finish_payload.args.detailed_report.clone();

    let related_units: Vec<ProjectUnitResponse> = if full_ids.is_empty() {
        vec![]
    } else {
        block_on_db_tokio(&pool, move |conn| {
            load_related_units_for_chat_session_by_full_ids(conn, project_id, session_id, &full_ids)
        })
        .await
        .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?
    };

    let merged = ToolAnalyzeArchitecturePayload {
        args: analyze_payload.args,
        related_units,
        detailed_report,
        tool_call_id: analyze_payload.tool_call_id,
        tool_name: Some(analyze_architecture_tool::TOOL_NAME.to_string()),
        error: None,
    };

    let merged_json = serde_json::to_value(&merged)
        .map_err(|e| -> BoxError { format!("serialize analyze payload: {}", e).into() })?;

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
                "finish_architecture_analysis: failed to publish message update: {}",
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
