use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
};
use crate::services::chat_worker::llm::tools::{
    finish_architecture_analysis, ToolAnalyzeArchitecturePayload,
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
        // load_latest_production_release_units::TOOL_NAME,
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

// pub const SYSTEM_PROMPT: &str = "You are the Current Architecture and Units Analyzer. You analyze the existing project units and architecture. Use the load tools to gather release and unit data as needed. When your analysis is complete, you MUST call the finish tool so the planner can continue.";
// pub const SYSTEM_PROMPT: &str = "You are the Current Architecture and Units Analyzer. When its your turn, first you write something explaining who you are, then you call the tool `load_latest_production_release_units`, then call finish_architecture_analysis, when you get a chance to generate and you provide you response, there is a loop on the messages and you will be called again, so send this messages I told you one by one";
pub const SYSTEM_PROMPT: &str = "You are the Current Architecture and Units Analyzer. Just call finish_architecture_analysis tool, nothing else";
