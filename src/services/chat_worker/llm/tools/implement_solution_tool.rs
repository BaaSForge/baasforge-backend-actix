use crate::database::{DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageType,
    ChatSessionRoleType, TextPayload,
};
use crate::models::projects::ProjectUnitResponse;
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::agent::{
    developer_tool_message_types, load_nested_subtree_descendant_rows,
};
use super::developer_crud_unit_ids;
use super::{BoxError, ToolSpec};
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use gemini_rust::tools::model::FunctionDeclaration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

type RedisConn = Arc<Mutex<redis::aio::MultiplexedConnection>>;

const SUBTREE_FALLBACK_ROW_CAP: usize = 100;

pub const ARG_DIRECTIVE_DESCRIPTION: &str =
    "Short orchestration handoff for the programmer: what this run must deliver, scope (units/areas or reference to agreed solution analysis), and non-negotiable constraints (e.g. production-ready, user-required routes/shapes, preserve existing contracts). Plain language only. Do NOT paste code, prescribe handlers/state/build order, or micromanage how to implement—the programmer decides those within their role.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolImplementSolutionArgs {
    #[schemars(description = ARG_DIRECTIVE_DESCRIPTION)]
    pub directive: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolImplementSolutionPayload {
    pub args: ToolImplementSolutionArgs,
    pub mutated_units: Vec<ProjectUnitResponse>,
    pub detailed_report: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete_mutated_units_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_detailed_report_fallback: Option<String>,
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolImplementSolution;

pub const TOOL_NAME: &str = "implement_solution_tool";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Run the programmer agent asynchronously to implement the agreed solution. Pass a short directive: outcomes, scope, and constraints only—the programmer owns implementation details. Never paste code or micromanage design.",
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
            mutated_units: vec![],
            detailed_report: String::new(),
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
            incomplete_mutated_units_ids: vec![],
            incomplete_detailed_report_fallback: None,
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

fn is_nested_branch_tool_result_type(t: &ChatMessageType) -> bool {
    developer_tool_message_types().contains(t)
}

fn collect_incomplete_mutated_unit_ids(rows: &[ChatMessageRow]) -> Vec<String> {
    let mut out = Vec::new();
    for row in rows {
        developer_crud_unit_ids::extend_unique_unit_ids_from_chat_tool_row(&mut out, row);
    }
    out
}

fn format_requested_tool_calls_block(tp: &TextPayload) -> Option<String> {
    let calls = tp.requested_tool_calls.as_ref()?.as_slice();
    if calls.is_empty() {
        return None;
    }
    let mut s = String::from("Agent has requested to call these tools and args:\n\n");
    for tc in calls {
        let args_json = serde_json::to_string(&tc.tool_arguments_to_pass).unwrap_or_else(|_| "{}".to_string());
        s.push_str("- ");
        s.push_str(&tc.tool_name);
        s.push_str(": ");
        s.push_str(&args_json);
        s.push('\n');
    }
    Some(s.trim_end().to_string())
}

fn format_tool_results_block(group: &[&ChatMessageRow]) -> String {
    let mut s = String::from("These are the tool results for previous tool calls:\n\n");
    for row in group {
        let name = tool_display_name_for_row(row);
        let body = row
            .payload
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| "{}".to_string());
        s.push_str("- ");
        s.push_str(&name);
        s.push_str(" result: ");
        s.push_str(&body);
        s.push('\n');
    }
    s.trim_end().to_string()
}

fn tool_display_name_for_row(row: &ChatMessageRow) -> String {
    if let Ok(Some(p)) = ChatMessagePayload::from_value(&row.type_, row.payload.clone()) {
        let n = match &p {
            ChatMessagePayload::ToolFetchRelevantUnits(p) => p.tool_name.as_deref(),
            ChatMessagePayload::ToolListUnitContexts(p) => p.tool_name.as_deref(),
            ChatMessagePayload::ToolListModulesForUnitContexts(p) => p.tool_name.as_deref(),
            ChatMessagePayload::ToolListProjectEnvKeys(p) => p.tool_name.as_deref(),
            ChatMessagePayload::ToolFinishImplementation(p) => p.tool_name.as_deref(),
            ChatMessagePayload::ToolCreateUnit(p) => p.tool_name.as_deref(),
            ChatMessagePayload::ToolModifyUnit(p) => p.tool_name.as_deref(),
            ChatMessagePayload::ToolRemoveUnit(p) => p.tool_name.as_deref(),
            _ => None,
        };
        if let Some(t) = n.filter(|s| !s.is_empty()) {
            return t.to_string();
        }
    }
    if let Some(v) = row.payload.as_ref().and_then(|p| p.get("tool_name").and_then(|x| x.as_str()))
    {
        if !v.is_empty() {
            return v.to_string();
        }
    }
    format!("{:?}", row.type_)
}

fn serialize_row_as_json_line(row: &ChatMessageRow) -> String {
    serde_json::to_string(&json!({
        "id": row.id,
        "role": row.role,
        "type": row.type_,
        "status": row.status,
        "agent": row.agent,
        "payload": row.payload,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

fn nested_rows_to_detailed_report_fallback(rows: &[ChatMessageRow]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < rows.len() {
        let row = &rows[i];
        if row.role == ChatSessionRoleType::Ai && row.type_ == ChatMessageType::Text {
            if let Some(tp) = row
                .payload
                .as_ref()
                .and_then(|v| serde_json::from_value::<TextPayload>(v.clone()).ok())
            {
                if let Some(block) = format_requested_tool_calls_block(&tp) {
                    parts.push(block);
                    i += 1;
                    continue;
                }
            }
            parts.push(serialize_row_as_json_line(row));
            i += 1;
        } else if row.role == ChatSessionRoleType::Ai && is_nested_branch_tool_result_type(&row.type_)
        {
            let start = i;
            i += 1;
            while i < rows.len()
                && rows[i].role == ChatSessionRoleType::Ai
                && is_nested_branch_tool_result_type(&rows[i].type_)
            {
                i += 1;
            }
            let group: Vec<&ChatMessageRow> = rows[start..i].iter().collect();
            parts.push(format_tool_results_block(&group));
        } else {
            parts.push(serialize_row_as_json_line(row));
            i += 1;
        }
    }
    parts.join("\n\n")
}

pub async fn merge_sub_agent_failure_fallback_into_tool_row(
    pool: &DbPool,
    redis_conn: &RedisConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    implement_tool_message_id: Uuid,
) {
    let implement_id = implement_tool_message_id;
    let updated_row = match block_on_db_tokio(pool, move |conn| {
        let rows = load_nested_subtree_descendant_rows(
            conn,
            session_id,
            implement_id,
            ChatMessageType::ToolImplementSolution,
            "implement_solution_tool",
            SUBTREE_FALLBACK_ROW_CAP,
        )?;
        let incomplete_ids = collect_incomplete_mutated_unit_ids(&rows);
        let fallback_report = nested_rows_to_detailed_report_fallback(&rows);
        let maybe_payload = chat_messages::table
            .filter(chat_messages::id.eq(implement_id))
            .filter(chat_messages::chat_session_id.eq(session_id))
            .select(chat_messages::payload)
            .first::<Option<Value>>(conn)
            .optional()?;
        let Some(Some(payload_val)) = maybe_payload else {
            return Ok(None);
        };
        let mut merged = match ChatMessagePayload::from_value(
            &ChatMessageType::ToolImplementSolution,
            Some(payload_val),
        ) {
            Ok(Some(ChatMessagePayload::ToolImplementSolution(p))) => p,
            _ => return Ok(None),
        };
        merged.incomplete_mutated_units_ids = incomplete_ids;
        merged.incomplete_detailed_report_fallback = if fallback_report.is_empty() {
            None
        } else {
            Some(fallback_report)
        };
        let merged_json = match serde_json::to_value(&merged) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    %implement_id,
                    "merge_sub_agent_failure_fallback_into_tool_row: serialize merged payload: {}",
                    e
                );
                return Ok(None);
            }
        };
        let row = diesel::update(chat_messages::table.filter(chat_messages::id.eq(implement_id)))
            .set(chat_messages::payload.eq(Some(merged_json)))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
            .optional()?;
        Ok::<_, diesel::result::Error>(row)
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                %implement_tool_message_id,
                "merge_sub_agent_failure_fallback_into_tool_row: {}",
                e
            );
            None
        }
    };
    let Some(row) = updated_row else {
        return;
    };
    let channel = user_channel(user_id);
    let message = ChatMessageResponse::from(row);
    let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
        project_id,
        session_id,
        message,
    });
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
        tracing::warn!(
            "merge_sub_agent_failure_fallback_into_tool_row: ws update failed: {}",
            e
        );
    }
}

#[cfg(test)]
mod nested_fallback_report_tests {
    use super::nested_rows_to_detailed_report_fallback;
    use crate::models::chat_messages::{
        ChatAgentType, ChatMessagePayload, ChatMessageRow, ChatMessageStatus, ChatMessageType,
        ChatSessionRoleType, RequestedToolCall, TextPayload,
    };
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    fn test_uuid(n: u8) -> Uuid {
        Uuid::parse_str(&format!(
            "10000000-0000-4000-8000-00000000{:04x}",
            n as u16
        ))
        .expect("test uuid")
    }

    const TEST_SESSION: &str = "20000000-0000-4000-8000-000000000001";

    fn row(
        n: u8,
        role: ChatSessionRoleType,
        type_: ChatMessageType,
        payload: Option<serde_json::Value>,
    ) -> ChatMessageRow {
        ChatMessageRow {
            id: test_uuid(n),
            chat_session_id: Uuid::parse_str(TEST_SESSION).expect("session uuid"),
            role,
            type_,
            payload,
            status: ChatMessageStatus::Completed,
            created_at: Utc::now(),
            invoking_message_id: None,
            agent: (role == ChatSessionRoleType::Ai).then_some(ChatAgentType::Developer),
            is_edit_disabled_due_to_live_production_release: false,
        }
    }

    fn list_tool_payload() -> serde_json::Value {
        list_tool_payload_with_slug("demo")
    }

    fn list_tool_payload_with_slug(slug: &str) -> serde_json::Value {
        serde_json::to_value(ChatMessagePayload::ToolListUnitContexts(
            crate::services::chat_worker::llm::tools::list_unit_contexts::ToolListUnitContextsPayload {
                args: Default::default(),
                context_slugs: vec![slug.to_string()],
                tool_call_id: Some("tc1".into()),
                tool_name: Some("list_unit_contexts".into()),
                error: None,
            },
        ))
        .expect("serialize list_unit_contexts payload")
    }

    #[test]
    fn empty_rows_yields_empty_string() {
        assert_eq!(nested_rows_to_detailed_report_fallback(&[]), "");
    }

    #[test]
    fn user_text_is_json_line() {
        let rows = vec![row(
            1,
            ChatSessionRoleType::User,
            ChatMessageType::Text,
            Some(json!({ "text": "hello" })),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(out.contains("hello"));
        assert!(out.contains(r#""role":"user""#) || out.contains(r#""role": "user""#));
        assert!(!out.contains("Agent has requested"));
    }

    #[test]
    fn ai_text_without_tool_calls_is_json_line() {
        let rows = vec![row(
            1,
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(json!({ "text": "status only" })),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(out.contains("status only"));
        assert!(out.contains(r#""role":"ai""#) || out.contains(r#""role": "ai""#));
        assert!(!out.contains("Agent has requested"));
    }

    #[test]
    fn ai_text_with_empty_requested_calls_array_serializes_as_json() {
        let rows = vec![row(
            1,
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(json!({ "text": "x", "requested_tool_calls": [] })),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(!out.contains("Agent has requested"));
        assert!(out.contains("x"));
    }

    #[test]
    fn ai_text_with_requested_tool_calls_uses_block_format() {
        let tp = TextPayload {
            text: "calling".into(),
            requested_tool_calls: Some(vec![RequestedToolCall {
                tool_call_id: Some("call-1".into()),
                tool_name: "list_unit_contexts".into(),
                tool_arguments_to_pass: json!({ "k": "v" }),
                thought_signature: None,
            }]),
            thinking_blocks: None,
        };
        let rows = vec![row(
            1,
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(serde_json::to_value(&tp).unwrap()),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(out.contains("Agent has requested to call these tools and args:"));
        assert!(out.contains("- list_unit_contexts:"));
        assert!(out.contains("\"k\":\"v\"") || out.contains(r#""k": "v""#));
    }

    #[test]
    fn single_developer_tool_result_block() {
        let rows = vec![row(
            1,
            ChatSessionRoleType::Ai,
            ChatMessageType::ToolListUnitContexts,
            Some(list_tool_payload()),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(out.contains("These are the tool results for previous tool calls:"));
        assert!(out.contains("list_unit_contexts result:"));
        assert!(out.contains("demo"));
    }

    #[test]
    fn consecutive_developer_tool_results_one_block() {
        let rows = vec![
            row(
                1,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload()),
            ),
            row(
                2,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListModulesForUnitContexts,
                Some(json!({
                    "args": { "context_slugs": ["demo"] },
                    "tool_name": "list_modules_for_unit_contexts",
                    "tool_call_id": "tc2"
                })),
            ),
        ];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert_eq!(out.matches("These are the tool results for previous tool calls:").count(), 1);
        assert!(out.contains("list_unit_contexts result:"));
        assert!(out.contains("list_modules_for_unit_contexts"));
    }

    #[test]
    fn non_developer_tool_row_is_json_not_grouped_with_crud() {
        let rows = vec![
            row(
                1,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolAnalyzeArchitecture,
                Some(json!({ "tool_name": "analyze_architecture_tool", "error": null })),
            ),
            row(
                2,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload()),
            ),
        ];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert_eq!(out.matches("These are the tool results for previous tool calls:").count(), 1);
        assert!(
            out.contains("tool_analyze_architecture")
                || out.contains("ToolAnalyzeArchitecture")
        );
        let idx_arch = out
            .find("tool_analyze_architecture")
            .or_else(|| out.find("ToolAnalyzeArchitecture"))
            .expect("architecture segment");
        let idx_results = out.find("These are the tool results").unwrap();
        assert!(idx_arch < idx_results);
    }

    #[test]
    fn mixed_sequence_joins_with_double_newline() {
        let tp = TextPayload {
            text: "step".into(),
            requested_tool_calls: Some(vec![RequestedToolCall {
                tool_call_id: None,
                tool_name: "create_unit".into(),
                tool_arguments_to_pass: json!({ "context_slug": "ctx" }),
                thought_signature: None,
            }]),
            thinking_blocks: None,
        };
        let rows = vec![
            row(
                1,
                ChatSessionRoleType::User,
                ChatMessageType::Text,
                Some(json!({ "text": "directive" })),
            ),
            row(
                2,
                ChatSessionRoleType::Ai,
                ChatMessageType::Text,
                Some(serde_json::to_value(&tp).unwrap()),
            ),
            row(
                3,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload()),
            ),
        ];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        let d = out.find("directive").expect("user text");
        let a = out
            .find("Agent has requested to call these tools and args:")
            .expect("tool-call block");
        let t = out
            .find("These are the tool results for previous tool calls:")
            .expect("tool results");
        assert!(d < a && a < t);
    }

    #[test]
    fn tool_block_split_by_ai_text_plain() {
        let rows = vec![
            row(
                1,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload()),
            ),
            row(
                2,
                ChatSessionRoleType::Ai,
                ChatMessageType::Text,
                Some(json!({ "text": "between" })),
            ),
            row(
                3,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload()),
            ),
        ];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert_eq!(
            out.matches("These are the tool results for previous tool calls:")
                .count(),
            2
        );
        assert!(out.contains("between"));
    }

    #[test]
    fn ai_text_malformed_payload_serializes_once_no_tool_call_block() {
        let rows = vec![row(
            1,
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(json!("not-a-text-payload-object")),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(!out.contains("Agent has requested"));
        assert_eq!(out.matches("These are the tool results").count(), 0);
    }

    #[test]
    fn ai_text_null_payload_serializes() {
        let rows = vec![row(
            1,
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            None,
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(!out.contains("Agent has requested"));
        assert!(out.contains("null"));
    }

    #[test]
    fn user_text_with_requested_calls_json_path_not_agent_block() {
        let rows = vec![row(
            1,
            ChatSessionRoleType::User,
            ChatMessageType::Text,
            Some(json!({
                "text": "u",
                "requested_tool_calls": [{ "tool_name": "create_unit", "tool_arguments_to_pass": {} }]
            })),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(!out.contains("Agent has requested"));
        assert!(out.contains("create_unit"));
    }

    #[test]
    fn three_consecutive_tools_preserve_row_order_in_single_block() {
        let rows = vec![
            row(
                1,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("zz_row0")),
            ),
            row(
                2,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("zz_row1")),
            ),
            row(
                3,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("zz_row2")),
            ),
        ];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert_eq!(out.matches("These are the tool results for previous tool calls:").count(), 1);
        let i0 = out.find("zz_row0").expect("row0 marker");
        let i1 = out.find("zz_row1").expect("row1 marker");
        let i2 = out.find("zz_row2").expect("row2 marker");
        assert!(i0 < i1 && i1 < i2, "tool result lines must follow row order, not reorder");
    }

    #[test]
    fn non_developer_ai_tool_breaks_inner_while_so_adjacent_dev_tools_split() {
        let rows = vec![
            row(
                1,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("gap_left_a")),
            ),
            row(
                2,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("gap_left_b")),
            ),
            row(
                3,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolAnalyzeArchitecture,
                Some(json!({ "tool_name": "analyze_architecture_tool" })),
            ),
            row(
                4,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("gap_right_c")),
            ),
        ];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert_eq!(out.matches("These are the tool results for previous tool calls:").count(), 2);
        let a = out.find("gap_left_a").unwrap();
        let b = out.find("gap_left_b").unwrap();
        let arch = out
            .find("tool_analyze_architecture")
            .or_else(|| out.find("ToolAnalyzeArchitecture"))
            .unwrap();
        let c = out.find("gap_right_c").unwrap();
        assert!(a < b && b < arch && arch < c);
    }

    #[test]
    fn user_row_between_ai_tool_rows_splits_two_result_blocks() {
        let rows = vec![
            row(
                1,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("split_before")),
            ),
            row(
                2,
                ChatSessionRoleType::User,
                ChatMessageType::Text,
                Some(json!({ "text": "wall" })),
            ),
            row(
                3,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("split_after")),
            ),
        ];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert_eq!(out.matches("These are the tool results for previous tool calls:").count(), 2);
        let before = out.find("split_before").unwrap();
        let wall = out.find("wall").unwrap();
        let after = out.find("split_after").unwrap();
        assert!(before < wall && wall < after);
    }

    #[test]
    fn system_role_text_uses_else_branch_json() {
        let rows = vec![row(
            1,
            ChatSessionRoleType::System,
            ChatMessageType::Text,
            Some(json!({ "text": "sys" })),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert!(out.contains("sys"));
        assert!(
            out.contains(r#""role":"system""#) || out.contains(r#""role": "system""#)
        );
        assert!(!out.contains("Agent has requested"));
    }

    #[test]
    fn alternating_tool_plain_text_tool_produces_three_separate_tool_blocks() {
        let rows = vec![
            row(
                1,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("alt_t0")),
            ),
            row(
                2,
                ChatSessionRoleType::Ai,
                ChatMessageType::Text,
                Some(json!({ "text": "plain_ai" })),
            ),
            row(
                3,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("alt_t1")),
            ),
            row(
                4,
                ChatSessionRoleType::Ai,
                ChatMessageType::Text,
                Some(json!({ "text": "plain_ai2" })),
            ),
            row(
                5,
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(list_tool_payload_with_slug("alt_t2")),
            ),
        ];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        assert_eq!(out.matches("These are the tool results for previous tool calls:").count(), 3);
        let p0 = out.find("plain_ai").unwrap();
        let p1 = out.find("plain_ai2").unwrap();
        let t0 = out.find("alt_t0").unwrap();
        let t1 = out.find("alt_t1").unwrap();
        let t2 = out.find("alt_t2").unwrap();
        assert!(t0 < p0 && p0 < t1 && t1 < p1 && p1 < t2);
    }

    #[test]
    fn one_text_row_multiple_requested_tools_order_matches_vector() {
        let tp = TextPayload {
            text: "multi".into(),
            requested_tool_calls: Some(vec![
                RequestedToolCall {
                    tool_call_id: None,
                    tool_name: "list_unit_contexts".into(),
                    tool_arguments_to_pass: json!({ "seq": 0 }),
                    thought_signature: None,
                },
                RequestedToolCall {
                    tool_call_id: None,
                    tool_name: "list_modules_for_unit_contexts".into(),
                    tool_arguments_to_pass: json!({ "seq": 1 }),
                    thought_signature: None,
                },
            ]),
            thinking_blocks: None,
        };
        let rows = vec![row(
            1,
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(serde_json::to_value(&tp).unwrap()),
        )];
        let out = nested_rows_to_detailed_report_fallback(&rows);
        let i0 = out.find("\"seq\":0").or_else(|| out.find(r#""seq": 0"#)).unwrap();
        let i1 = out.find("\"seq\":1").or_else(|| out.find(r#""seq": 1"#)).unwrap();
        assert!(i0 < i1);
        assert!(out.find("- list_unit_contexts:").unwrap() < out.find("- list_modules_for_unit_contexts:").unwrap());
    }
}
