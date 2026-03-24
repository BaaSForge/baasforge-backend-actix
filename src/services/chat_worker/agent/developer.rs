use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatMessageType, ChatSessionRoleType,
};
use crate::schema::chat_messages;
use crate::services::chat_worker::agent::nested_branch_history::load_nested_branch_history;
use crate::services::chat_worker::llm::tools::{
    create_unit, finish_implementation, modify_unit, remove_unit,
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
        create_unit::TOOL_NAME,
        modify_unit::TOOL_NAME,
        remove_unit::TOOL_NAME,
        finish_implementation::TOOL_NAME,
    ]
}

/// Loads history **only** under the nested branch rooted at the `ToolImplementSolution` DB row.
///
/// Prepends a synthetic user message: tool `directive` from the implement row (solution context is in planner history).
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

const SYSTEM_PROMPT_CORE: &str = r#"Role: You are the Programmer for a "No-Code" backend platform. You implement units: create or modify the exact units requested. You do not replan, justify trade-offs, or restate the agreed solution—the synthetic user message gives you the directive.

1. Behavior
- Use only your tools. Do not deliver long prose; short status between tool calls is fine.
- Prefer new units over edits when both work; if you must change an existing unit, preserve behavior outside the requested delta.
- **Code language:** All implementation you put in the `code` field of create unit and modify unit must be **JavaScript**.
- **modify_unit:** You cannot change the unit kind when revising; the new draft row keeps the parent unit’s type. To use a different kind, create a new unit with create_unit instead. Pass `unit_id` as the exact `id` field (a full hyphenated UUID), not a label or paraphrase or inside any backticks or double quotes.
- **remove_unit:** Soft-removes a unit by inserting a new draft revision with empty code, explanation copied from the parent revision, is_deleted true, and a major semver bump (breaking). Same `unit_id` rules as modify_unit. Use when the directive asks to delete or drop a unit.

2. Tool use
Do not repeat identical tool calls.

3. Execution loop
When you need data, issue tool_calls and wait for results. During the loop you may iterate and leave short status lines in assistant text—that is normal. When implementation is complete, you MUST call finish implementation—never end this run without that tool.

4. Ending
Your final action is always finish implementation with the argument detailed report. Write detailed report only after all unit tools are done. It must be a full, self-contained report for the planner and user: which units were created, modified, or removed (identities and intended behavior), how that satisfies the directive, and any caveats—not a shortened recap.

In detailed report, do not include trial-and-error narratives, dead ends, or step-by-step exploration—only the consolidated outcome and what matters for the project going forward."#;

pub static SYSTEM_PROMPT_WITH_UNIT_DETAILS: LazyLock<&'static str> = LazyLock::new(|| {
    let combined = format!(
        "{SYSTEM_PROMPT_CORE}\n\n## Unit type reference (detailed)\n\n{}",
        units_definitions::all_details(),
    );
    Box::leak(combined.into_boxed_str())
});
