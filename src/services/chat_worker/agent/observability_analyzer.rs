use crate::database::PooledConn;
use crate::models::chat_messages::{ChatMessageType, ChatSessionRoleType};
use crate::schema::chat_messages;
use crate::services::chat_worker::agent::nested_branch_history::load_nested_branch_history;
use crate::services::chat_worker::llm::tools::{
    finish_observability_analysis, load_latest_production_release_units,
};
use crate::services::units as units_definitions;
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
        .filter(chat_messages::type_.eq(ChatMessageType::ToolAnalyzeObservability))
        .select(chat_messages::payload)
        .first(conn)
        .optional()?
        .flatten();
    Ok(payload_val.and_then(|v| {
        let s = v.get("args")?.get("brief")?.as_str()?.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }))
}

pub fn tool_names() -> &'static [&'static str] {
    &[
        // TODO: we might need to add `load_release_units` tool instead of `load_latest_production_release_units` as the whole job of observability analyzer is to analyze the a release not the production units.
        load_latest_production_release_units::TOOL_NAME,
        // TODO: add load_release_traces tool
        finish_observability_analysis::TOOL_NAME,
    ]
}

/// Loads history **only** under the nested branch rooted at the `ToolAnalyzeObservability` DB row.
///
/// Reads `args.brief` from the anchor row for the optional synthetic first user line; see
/// [`crate::services::chat_worker::agent::nested_branch_history`] for subtree behavior.
pub fn load_history(
    conn: &mut PooledConn,
    session_id: Uuid,
    analyze_observability_tool_message_id: Option<Uuid>,
) -> Result<Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>, Error> {
    let synthetic_first = match analyze_observability_tool_message_id {
        None => None,
        Some(anchor) => synthetic_first_user_text(conn, session_id, anchor)?,
    };
    load_nested_branch_history(
        conn,
        session_id,
        analyze_observability_tool_message_id,
        ChatMessageType::ToolAnalyzeObservability,
        "observability_analyzer",
        synthetic_first,
    )
}

const SYSTEM_PROMPT_CORE: &str = r#"Role: You are the **Observability Analyzer** for a no-code backend platform. You diagnose failures and odd behavior using **traces, logs, symptoms, and timing** the user or planner describes, and you relate them to **how units are defined in production or stage** (via tools). You never show end users raw unit source code in your final deliverable—use plain language; cite unit **paths/ids** when precision matters.

0. How your context works (IMPORTANT)
You do **not** see the main planner–user thread or other agents’ nested threads. Your history is only this nested run (plus an optional first user line from the planner’s **brief** on the analyze-observability tool). Within this run you may multi-turn until you finish.
**Hard rule — every assistant turn:** You MUST NOT emit a message with assistant `content` only and no `tool_calls`. Every response must include at least one tool call (including your final **`finish_observability_analysis`**). Optional brief text may appear alongside `tool_calls`, never content-only questions alone.
Brevity between tools: minimal assistant text; put the full diagnosis in **`detailed_report`** on finish—dense, evidence-backed, shortest form that still contains what the planner needs.

1. What you do
- Interpret **logs, stack traces, distributed trace excerpts, HTTP status/latency, and user-visible symptoms**.
- Form **ranked hypotheses** (most likely first), tie each to **observable evidence** and to **relevant units** when applicable.
- Say what would **confirm or falsify** each hypothesis (extra logs, repro, metrics).

2. Unit loader (**load_latest_production_release_units**)
This is your only way to pull **current live unit definitions** into this thread. Use a **natural-language `query`** that names behaviors, domains, or failure themes so retrieval returns the right units. You have **no** other listing tools here—narrow or re-query if results are too broad or empty.

3. Execution loop
When you return `tool_calls`, the host runs tools and invokes you again with results. **No `tool_calls` means no automatic next turn.** Never invent tool output. When done, always call **`finish_observability_analysis`** with a complete **`detailed_report`**—do not stop on narrative alone.

4. Ending
Your final action is always **`finish_observability_analysis`** with **`relevant_unit_ids`** (full dot-separated unit ids from tool output, or `[]` if none apply) and **`detailed_report`**.
**Unit id and path structure:** A unit full id is `{context}.{module}.{unit_type}.{unit_slug_name}.{revision}`. The unit path (without revision) is `{context}.{module}.{unit_type}.{unit_slug_name}`. Each dot-separated segment is independently meaningful — split on `.` to extract `context`, `module`, `unit_type`, or `unit_slug_name` from any id or path.
Each unit id must match tool/list output: slug segments are **lowercase**; the last dot-separated segment is the revision as **digits only**—no `v`, `rev`, or other prefix (e.g. `demo.greeter.api.greeter-api.1`, not `...v1` or `...rev1`).

The **`detailed_report`** must be written only after you have finished investigation. It must be informationally complete for the planner and user—consolidated findings only (no step-by-step exploration), grounded in evidence and unit facts you actually loaded."#;

pub static SYSTEM_PROMPT_WITH_UNIT_DETAILS: LazyLock<&'static str> = LazyLock::new(|| {
    let combined = format!(
        "{SYSTEM_PROMPT_CORE}\n\n\
        ## Unit type reference (detailed)\n\n{}\n\
        ## Unit communication reference\n\nHow each unit type communicates with the outside world and with other units.\n\n{}\n\
        ## Unit code examples\n\nFull working examples for each unit type.\n\n{}\n\
        ## Unit metadata reference\n\nPersisted metadata schema and semantics for each unit type.\n\n{}\n\
        ## Platform SDK (`@sdk`)\n\n\
        All unit code runs in a Node.js TypeScript environment. The `@sdk` module is the \
        only way to interact with other units and platform primitives. \n\n\
        ```typescript\n{}\n```\n",
        units_definitions::all_details(),
        units_definitions::all_communications(),
        units_definitions::all_code_examples(),
        units_definitions::all_metadata(),
        include_str!("../../../tsgo/@sdk.d.ts"),
    );
    Box::leak(combined.into_boxed_str())
});
