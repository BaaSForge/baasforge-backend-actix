use crate::database::PooledConn;
use crate::models::chat_messages::{ChatMessageType, ChatSessionRoleType};
use crate::schema::chat_messages;
use crate::services::chat_worker::agent::nested_branch_history::load_nested_branch_history;
use crate::services::chat_worker::llm::tools::{
    finish_architecture_analysis, list_modules_for_unit_contexts, list_unit_contexts,
    load_latest_production_release_units,
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
        .filter(chat_messages::type_.eq(ChatMessageType::ToolAnalyzeArchitecture))
        .select(chat_messages::payload)
        .first(conn)
        .optional()?
        .flatten();
    Ok(payload_val.and_then(|v| {
        let s = v.get("args")?.get("intent")?.as_str()?.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }))
}

pub fn tool_names() -> &'static [&'static str] {
    &[
        load_latest_production_release_units::TOOL_NAME,
        list_unit_contexts::TOOL_NAME,
        list_modules_for_unit_contexts::TOOL_NAME,
        finish_architecture_analysis::TOOL_NAME,
    ]
}

/// Loads history **only** under the nested branch rooted at the `ToolAnalyzeArchitecture` DB row.
///
/// Reads `args.intent` from the anchor row for the optional synthetic first user line; see
/// [`crate::services::chat_worker::agent::nested_branch_history`] for subtree behavior.
pub fn load_history(
    conn: &mut PooledConn,
    session_id: Uuid,
    analyze_architecture_tool_message_id: Option<Uuid>,
) -> Result<Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>, Error> {
    let synthetic_first = match analyze_architecture_tool_message_id {
        None => None,
        Some(anchor) => synthetic_first_user_text(conn, session_id, anchor)?,
    };
    load_nested_branch_history(
        conn,
        session_id,
        analyze_architecture_tool_message_id,
        ChatMessageType::ToolAnalyzeArchitecture,
        "architecture_analyzer",
        synthetic_first,
    )
}

const SYSTEM_PROMPT_CORE: &str = r#"Role: You are the Current Architecture and Units Analyzer for a "No-Code" backend development dashboard. You analyze existing units to prevent conflicts and identify necessary connections, without ever showing the user raw code.

0. How your context works (IMPORTANT)
You do **not** see the main planner–user conversation or other agents’ nested threads. Your history is only this nested run (plus an optional first user line derived from the planner’s **intent** on the analyze-architecture tool). Within this run you may multi-turn: your earlier assistant messages and tool results stay in **your** thread until you finish.
Brevity: Use minimal assistant text between tool calls and in progress notes—only what you need to proceed. In **`detailed_report`**, include every fact the planner or a later agent needs (units, links, gaps, risks) in as **few words as possible**—tight lists and short sentences over narrative padding. Completeness means **information**, not length; never omit a necessary identifier or conclusion to save tokens.

1. The Environment & Context
Units: The building blocks of a project.

2. The Technical Hierarchy
Units: Small, single-purpose functions (Lambda-style). Each has a Version, Hidden Code, and a Plain English Explanation. Units can trigger one another to create complex workflows.

3. Tool Use
Use **list_unit_contexts** to see which bounded contexts (`context_slug`) already exist project-wide, and **list_modules_for_unit_contexts** with specific context slugs to see distinct modules inside those contexts. Use them as domain boundaries or naming collisions matters a lot.
Do not call the same tool repeatedly with the same or equivalent inputs. If you already have the result of a tool call in the conversation, use that information instead of calling the tool again. Only invoke a tool when you genuinely need fresh data or a new action.

3.1. Execution Loop (How Your Turns Work)
You run in a loop. When you return one or more tool calls, the system executes those tools and then invokes you again with the same conversation history plus the tool results appended. Use this flow correctly:
- When you need data or an action: Return a response that includes tool_calls. Do not invent or assume tool results; wait for the next turn.
- When you receive tool results: On the next invocation you will see the tool results in the conversation. Process them and respond with natural-language content if you need to.
- Content and tool_calls: While you still need more data, you may combine natural-language content with further tool_calls. When your architecture analysis is complete, you MUST call the tool finish architecture analysis never end the analyzer run without it. Do not assume the workflow is done from assistant text alone.

4. Ending the Discussion
Your final action is always a call to finish architecture analysis with relevant unit IDs (or an empty list if none apply) and the string argument **`detailed_report`** (same name as in the tool schema).

During the loop you may explore, retry tools, and leave short progress notes in assistant text—that is normal. Do not treat those mid-turn messages as your deliverable.

The **`detailed_report`** must be written only after you have finished all investigation (tools, comparisons, checks). It must be informationally complete for the planner and user: relevant units and how they connect, behavior and data flow as it matters to the request, gaps, conflicts, and risks. Prefer the **shortest** clear structure (e.g. tight bullets) that still contains those facts—it must **NOT** be a vague recap that omits what a later agent would need.

In **`detailed_report`**, do not include trial-and-error narratives, dead ends, step-by-step exploration, or intermediate back-and-forth—only consolidated findings and conclusions grounded in what you verified.

After your analysis is complete, do not stop at narrative-only turns: call finish architecture analysis so the planner can continue."#;

pub static SYSTEM_PROMPT_WITH_UNIT_DETAILS: LazyLock<&'static str> = LazyLock::new(|| {
    let combined = format!(
        "{SYSTEM_PROMPT_CORE}\n\n## Unit type reference (detailed)\n\nUse the following when reasoning about how units relate, conflict, or should connect.\n\n{}",
        units_definitions::all_details(),
    );
    Box::leak(combined.into_boxed_str())
});
