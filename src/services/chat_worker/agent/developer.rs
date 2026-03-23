use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatMessageType, ChatSessionRoleType,
};
use crate::schema::chat_messages;
use crate::services::chat_worker::agent::nested_branch_history::load_nested_branch_history;
use crate::services::chat_worker::llm::tools::{
    finish_implementation, load_latest_production_release_units,
};
use crate::services::chat_worker::units_definitions;
use diesel::prelude::*;
use diesel::result::Error;
use diesel::OptionalExtension;
use serde_json::Value;
use std::sync::LazyLock;
use uuid::Uuid;

fn synthetic_first_user_text(
    conn: &mut PooledConn,
    session_id: Uuid,
    anchor: Uuid,
) -> Result<Option<String>, Error> {
    let payload_val: Option<Value> = chat_messages::table
        .filter(chat_messages::id.eq(anchor))
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::type_.eq(ChatMessageType::ToolImplementSolution))
        .select(chat_messages::payload)
        .first(conn)
        .optional()?
        .flatten();
    let Some(directive) = payload_val.and_then(|v| {
        v.get("args")?
            .get("directive")?
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }) else {
        return Ok(None);
    };
    let mut out = String::new();
    out.push_str("## Directive (execute exactly)\n\n");
    out.push_str(&directive);
    Ok(Some(out))
}

pub fn tool_names() -> &'static [&'static str] {
    &[
        load_latest_production_release_units::TOOL_NAME,
        finish_implementation::TOOL_NAME,
    ]
}

/// Loads history **only** under the nested branch rooted at the `ToolImplementSolution` DB row.
///
/// Prepends a synthetic user message: latest completed solution summary (if any) plus the tool `directive`.
pub fn load_history(
    conn: &mut PooledConn,
    session_id: Uuid,
    implement_solution_tool_message_id: Option<Uuid>,
) -> Result<Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>, Error> {
    let synthetic_first = match implement_solution_tool_message_id {
        None => None,
        Some(anchor) => synthetic_first_user_text(conn, session_id, anchor)?,
    };
    load_nested_branch_history(
        conn,
        session_id,
        implement_solution_tool_message_id,
        ChatMessageType::ToolImplementSolution,
        "developer",
        synthetic_first,
    )
}

const SYSTEM_PROMPT_CORE: &str = r#"Role: You are the Programmer for a "No-Code" backend platform. You implement units: create or modify the exact units requested. You do not replan, justify trade-offs, or restate the agreed solution—the user message gives you the analysis summary and a directive; follow both.

1. Behavior
- Use only your tools. Do not deliver long prose; short status between tool calls is fine.
- Prefer new units over edits when both work; if you must change an existing unit, preserve behavior outside the requested delta.

2. Tool use
Do not repeat identical tool calls.

3. Execution loop
When you need data, issue tool_calls and wait for results. When implementation is complete, you MUST call finish implementation with a concise summary—never end this run without that tool.

4. Ending
Your final action is always finish implementation with an implementation_summary for the planner (what was created or changed)."#;

pub static SYSTEM_PROMPT_WITH_UNIT_DETAILS: LazyLock<&'static str> = LazyLock::new(|| {
    let combined = format!(
        "{SYSTEM_PROMPT_CORE}\n\n## Unit type reference (detailed)\n\n{}",
        units_definitions::all_details(),
    );
    Box::leak(combined.into_boxed_str())
});
