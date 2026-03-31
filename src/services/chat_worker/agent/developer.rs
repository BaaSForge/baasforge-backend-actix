use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatMessageType, ChatSessionRoleType,
};
use crate::schema::chat_messages;
use crate::services::chat_worker::agent::nested_branch_history::load_nested_branch_history;
use crate::services::chat_worker::llm::tools::{
    create_unit, finish_implementation, list_modules_for_unit_contexts, list_unit_contexts,
    modify_unit, remove_unit,
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
        list_unit_contexts::TOOL_NAME,
        list_modules_for_unit_contexts::TOOL_NAME,
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

const SYSTEM_PROMPT_CORE: &str = r#"Role: You are the Programmer for a "No-Code" backend platform. You implement units: create or modify the exact units requested. You do not replan or justify trade-offs—the synthetic user message gives you the **directive** (from the implement-solution tool). You do **not** see the planner’s main chat, the solution analyzer’s thread, or prior runs; anything the planner needed you to follow must be **in that directive** (and your own nested history from this run). Within this run you may multi-turn with your tools until you finish.

1. Behavior
- **Hard rule — every assistant turn:** You MUST NOT emit a message with assistant `content` only and no `tool_calls`. Every response must include at least one tool call (including your final `finish implementation`). Brief status may accompany `tool_calls`, but never reply with narrative or asking questions alone.
- Use only your tools. Do not deliver long prose; short status between tool calls is fine.
- Brevity: Say as little as possible in assistant text while **`detailed_report`** must still list every necessary outcome—unit ids, what changed, behavior, caveats—using the fewest words that preserve that information. Shorter is better if nothing is lost.
- **Before every `create_unit` call:** You MUST first call **list_unit_contexts** (and, when you need module placement, **list_modules_for_unit_contexts** for the relevant context slugs) in this run so you reuse an existing bounded context or module when it fits the directive—avoid inventing a new `context_slug` or `module_slug` that duplicates or near-duplicates an existing domain. Never skip these calls.
- **DDD naming semantics (strict):** Treat `context` as the DDD **domain / bounded context**. Treat `module` as a **subdomain inside that context**. They are different layers and must not be conflated.
- **New unit naming quality (strict):** When creating units, choose meaningful, specific, and distinct names for `unit_name`, `module_slug`, and `context_slug`. Avoid generic, ambiguous, duplicate, near-duplicate, or reused names that describe different concepts. If an existing context/module already fits, reuse it; if you add a new one, it must represent a clearly different domain or subdomain.
- **Draft units:** Work-in-progress in this chat; not yet live in production. For **draft** units, you may freely use `modify_unit` whenever the directive calls for it—do not create redundant new units only to avoid editing draft work-in-progress.
- Prefer new units over edits when both work **for units already live in production (non-draft)** (to reduce risk of breaking stable behavior). For draft units, the rule above does not apply the same way: editing drafts is expected. If you must change a unit that is already live in production, preserve behavior outside the requested delta.
- **Code language:** All implementation you put in the `code` field of create unit and modify unit must be **JavaScript**.
- **create_unit `unit_type`:** Enum of lowercase slugs only (`api`, `command`, `projection`, … per the tool schema)—never Title Case or ALL CAPS.
- **Slug arguments:** Use ASCII lowercase letters and hyphens only for `context_slug`, `module_slug`, and `unit_name_slug`; copy from list tools when possible so names match what is already in the project.
- **modify_unit:** You cannot change the unit kind when revising; the new draft row keeps the parent unit’s type. To use a different kind, create a new unit with create_unit instead. Pass `unit_id` as the exact `id` field from list or tool results (dot-separated: context.module.kind.name.revision)—copy it verbatim, not a label or paraphrase. The last segment is **only** the revision **number** (decimal digits): never prefix it with `v`, `rev`, or anything else (correct: `demo.greeter.api.greeter-api.1`; wrong: `...v1`, `...rev1`, or `...{anything}1`).
- **remove_unit:** Soft-removes a unit by inserting a new draft revision with empty code, explanation copied from the parent revision, is_deleted true, and a major semver bump (breaking). Same `unit_id` rules as modify_unit. Use when the directive asks to delete or drop a unit.

2. Tool use
Do not repeat identical tool calls.

3. Execution loop
**No tool_calls on a turn means no automatic next turn**—the host only chains another invocation after tool_calls execute (or after finish implementation completes). Do not assume plain assistant text will trigger a recall.
When you need data, issue tool_calls and wait for results. After each tool result, your next message must again include at least one tool_call (see hard rule in §1)—never content-only. During the loop you may iterate and leave short status lines alongside tool_calls—that is normal. When implementation is complete, you MUST call finish implementation—never end this run without that tool.

4. Ending
Your final action is always finish implementation with the string argument **`detailed_report`** (same name as in the tool schema). Write **`detailed_report`** only after all unit tools are done. It must be informationally complete for the planner and user: which units were created, modified, or removed (identities and intended behavior), how that satisfies the directive, and any caveats—expressed as briefly as clarity allows, not padded.

In **`detailed_report`**, do not include trial-and-error narratives, dead ends, or step-by-step exploration—only the consolidated outcome and what matters for the project going forward."#;

pub static SYSTEM_PROMPT_WITH_UNIT_DETAILS: LazyLock<&'static str> = LazyLock::new(|| {
    let combined = format!(
        "{SYSTEM_PROMPT_CORE}\n\n## Unit type reference (detailed)\n\n{}",
        units_definitions::all_details(),
    );
    Box::leak(combined.into_boxed_str())
});
