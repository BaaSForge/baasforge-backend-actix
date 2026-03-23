use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
};
use crate::services::chat_worker::llm::tools::{
    ToolAnalyzeArchitecturePayload, finish_architecture_analysis, load_latest_production_release_units
};
use crate::schema::chat_messages;
use diesel::prelude::*;
use diesel::result::Error;
use diesel::OptionalExtension;
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

const HISTORY_LIMIT: i64 = 50;

/// All message ids in the subtree rooted at `anchor` (children follow `invoking_message_id` edges).
fn subtree_message_ids(
    conn: &mut PooledConn,
    session_id: Uuid,
    anchor: Uuid,
) -> Result<Vec<Uuid>, Error> {
    let mut seen: HashSet<Uuid> = HashSet::new();
    seen.insert(anchor);
    let mut frontier = vec![anchor];
    loop {
        if frontier.is_empty() {
            break;
        }
        let children: Vec<Uuid> = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(chat_messages::invoking_message_id.eq_any(&frontier))
            .select(chat_messages::id)
            .load(conn)?;
        frontier.clear();
        for id in children {
            if seen.insert(id) {
                frontier.push(id);
            }
        }
    }
    let mut ids: Vec<Uuid> = seen.into_iter().collect();
    ids.sort();
    Ok(ids)
}

/// First user turn for the nested analyzer: `args.intent` from the `ToolAnalyzeArchitecture` row.
/// Read **without** requiring `Completed` so the first LLM call still gets contents while the tool row is in flight.
fn synthetic_intent_user_message(
    conn: &mut PooledConn,
    session_id: Uuid,
    anchor: Uuid,
) -> Result<Option<(ChatSessionRoleType, ChatMessageType, Option<Value>)>, Error> {
    let payload_val: Option<Value> = chat_messages::table
        .filter(chat_messages::id.eq(anchor))
        .filter(chat_messages::chat_session_id.eq(session_id))
        .select(chat_messages::payload)
        .first(conn)
        .optional()?
        .flatten();

    let Some(v) = payload_val else {
        return Ok(None);
    };
    let Ok(p) = serde_json::from_value::<ToolAnalyzeArchitecturePayload>(v) else {
        return Ok(None);
    };
    let intent = p.args.intent.trim().to_string();
    if intent.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        ChatSessionRoleType::User,
        ChatMessageType::Text,
        Some(serde_json::json!({ "text": intent })),
    )))
}

pub fn tool_names() -> &'static [&'static str] {
    &[
        load_latest_production_release_units::TOOL_NAME,
        finish_architecture_analysis::TOOL_NAME,
    ]
}

/// Loads history **only** under the nested branch rooted at the `ToolAnalyzeArchitecture` DB row.
///
/// A synthetic **User** message with the tool’s `args.intent` is always prepended when non-empty so
/// the first analyzer call still has upstream contents (subtree rows use `Completed` only; the anchor
/// tool row is usually still pending on that first pass).
///
/// **`analyze_architecture_tool_message_id`** must be the id of that tool message (same value as
/// `ChatSessionQueueMessage::invoking_message_id` / the analyzer AI Text row’s parent). If missing or
/// not a `ToolAnalyzeArchitecture` row in this session, returns an empty history (no main-thread messages).
pub fn load_history(
    conn: &mut PooledConn,
    session_id: Uuid,
    analyze_architecture_tool_message_id: Option<Uuid>,
) -> Result<Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>, Error> {
    let Some(anchor) = analyze_architecture_tool_message_id else {
        return Ok(vec![]);
    };

    let tool_ok = chat_messages::table
        .filter(chat_messages::id.eq(anchor))
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::type_.eq(ChatMessageType::ToolAnalyzeArchitecture))
        .select(chat_messages::id)
        .first::<Uuid>(conn)
        .optional()?
        .is_some();

    if !tool_ok {
        tracing::warn!(
            %anchor,
            %session_id,
            "architecture_analyzer::load_history: expected id of ToolAnalyzeArchitecture row in session; returning empty history"
        );
        return Ok(vec![]);
    }

    let intent_prefix = synthetic_intent_user_message(conn, session_id, anchor)?;

    let ids = subtree_message_ids(conn, session_id, anchor)?;
    let rows: Vec<ChatMessageRow> = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::id.eq_any(ids))
        .filter(chat_messages::status.eq(ChatMessageStatus::Completed))
        .order(chat_messages::id.asc())
        .select(ChatMessageRow::as_select())
        .load(conn)?;

    let matches_agent_filter = |row: &ChatMessageRow| match row.role {
        ChatSessionRoleType::User => true,
        ChatSessionRoleType::Ai => {
            matches!(
                row.type_,
                ChatMessageType::ToolFetchRelevantUnits
                    | ChatMessageType::ToolFetchLatestReleases
                    | ChatMessageType::ToolAnalyzeArchitecture
                    | ChatMessageType::ToolFinishArchitectureAnalysis
            ) || row.agent.as_ref() == Some(&ChatAgentType::ArchitectureAnalyzer)
                || row.agent.is_none()
        }
    };

    let mut mapped: Vec<_> = rows
        .into_iter()
        .filter(matches_agent_filter)
        .map(|row| (row.role, row.type_, row.payload))
        .collect();

    if mapped.len() > HISTORY_LIMIT as usize {
        let start = mapped.len() - HISTORY_LIMIT as usize;
        mapped.drain(..start);
    }

    if let Some(prefix) = intent_prefix {
        mapped.insert(0, prefix);
        while mapped.len() > HISTORY_LIMIT as usize {
            if mapped.len() <= 1 {
                break;
            }
            mapped.remove(1);
        }
    }

    Ok(mapped)
}

pub const SYSTEM_PROMPT: &str = r#"Role: You are the Current Architecture and Units Analyzer for a "No-Code" backend development dashboard. You analyze existing units to prevent conflicts and identify necessary connections, without ever showing the user raw code.

1. The Environment & Context
Units: The building blocks of a project.

2. The Technical Hierarchy
Units: Small, single-purpose functions (Lambda-style). Each has a Version, Hidden Code, and a Plain English Explanation. Units can trigger one another to create complex workflows.

3. Tool Use
Do not call the same tool repeatedly with the same or equivalent inputs. If you already have the result of a tool call in the conversation, use that information instead of calling the tool again. Only invoke a tool when you genuinely need fresh data or a new action.

3.1. Execution Loop (How Your Turns Work)
You run in a loop. When you return one or more tool calls, the system executes those tools and then invokes you again with the same conversation history plus the tool results appended. Use this flow correctly:
- When you need data or an action: Return a response that includes tool_calls. Do not invent or assume tool results; wait for the next turn.
- When you receive tool results: On the next invocation you will see the tool results in the conversation. Process them and respond with natural-language content if you need to.
- Content and tool_calls: While you still need more data, you may combine natural-language content with further tool_calls. When your architecture analysis is complete, you MUST call the tool `finish_architecture_analysis`—never end the analyzer run without it. Do not assume the workflow is done from assistant text alone.

4. Ending the Discussion
Your final action in this analyzer session is always a call to `finish_architecture_analysis` with relevant unit IDs (or an empty list if none apply) and a concise summary for the planner and user. After you have gathered enough information, do not stop at narrative-only turns: call `finish_architecture_analysis` so the planner can continue. Until then, you may use assistant content to explain progress between tool calls."#;
