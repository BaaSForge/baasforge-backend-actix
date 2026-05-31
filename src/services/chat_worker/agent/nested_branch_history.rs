//! Shared history loading for nested sub-agents whose conversation hangs under an async tool row
//! (e.g. `ToolAnalyzeArchitecture`, `ToolAnalyzeSolution`, `ToolAnalyzeObservability`, `ToolImplementSolution`).
//!
//! These subtrees are **not** visible to the Planner’s other messages or to other agents’ subtrees;
//! each invocation is a separate branch. The Planner must **refine** prior `detailed_report` text
//! into **intent** / **directive** (planner §2.1.1; [`crate::services::chat_worker::agent::load_history_for_agent`]).
//!
//! History is scoped by the **subtree** rooted at the anchor tool row (`invoking_message_id`
//! edges). The **anchor row itself is not replayed**: it is the planner-side tool invocation; the
//! matching `tool_use` lives on the planner assistant message (outside this branch). Including the
//! anchor as a `ToolResult` would break APIs (e.g. Claude) that require a preceding assistant
//! `tool_use` with the same id. Only **descendants** of the anchor are loaded.
//!
//! ## Synthetic leading user message
//! Pass `synthetic_first_user_text`: if `Some` and non-empty after trim, it is prepended as a
//! synthetic **User** `Text` message (same shape as a normal user turn). The caller decides the
//! string (e.g. after reading the anchor row). Use `None` to omit.

use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
};
use crate::schema::chat_messages;
use diesel::OptionalExtension;
use diesel::prelude::*;
use diesel::result::Error;
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

const HISTORY_LIMIT: i64 = 50;

#[derive(Debug)]
pub struct SessionHistoryLoad {
    pub session_messages: Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>,
    pub db_rows_chronological: Vec<ChatMessageRow>,
}

/// All message ids in the subtree rooted at `anchor` (children follow `invoking_message_id` edges).
pub fn subtree_message_ids(
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

/// Descendant [`ChatMessageRow`]s under `anchor` (excluding the anchor row), ordered by id ascending.
/// Only **Completed**, **Failed**, and **Stopped** rows (excludes **Generating**), then tail-trimmed to at most `row_cap` rows.
pub fn load_nested_subtree_descendant_rows(
    conn: &mut PooledConn,
    session_id: Uuid,
    anchor: Uuid,
    expected_anchor_type: ChatMessageType,
    warn_label: &'static str,
    row_cap: usize,
) -> Result<Vec<ChatMessageRow>, Error> {
    let tool_ok = chat_messages::table
        .filter(chat_messages::id.eq(anchor))
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::type_.eq(expected_anchor_type))
        .select(chat_messages::id)
        .first::<Uuid>(conn)
        .optional()?
        .is_some();

    if !tool_ok {
        tracing::warn!(
            %anchor,
            %session_id,
            "{}::load_nested_subtree_descendant_rows: anchor not found or wrong type in session",
            warn_label
        );
        return Ok(vec![]);
    }

    let ids = subtree_message_ids(conn, session_id, anchor)?;
    let ids: Vec<Uuid> = ids.into_iter().filter(|id| *id != anchor).collect();
    if ids.is_empty() {
        return Ok(vec![]);
    }

    let mut rows: Vec<ChatMessageRow> = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::id.eq_any(&ids))
        .filter(
            chat_messages::status
                .eq(ChatMessageStatus::Completed)
                .or(chat_messages::status.eq(ChatMessageStatus::Failed))
                .or(chat_messages::status.eq(ChatMessageStatus::Stopped)),
        )
        .order(chat_messages::id.asc())
        .select(ChatMessageRow::as_select())
        .load(conn)?;

    if rows.len() > row_cap {
        let start = rows.len() - row_cap;
        rows.drain(..start);
    }
    Ok(rows)
}

/// Loads history under the nested branch rooted at the given tool message id:
/// **Completed**, **Failed**, and **Stopped** rows only (excludes **Generating**) so assistant `tool_use` turns,
/// tool validation failures, and user-stopped runs still pair with their tool results on replay.
///
/// When `anchor_tool_message_id` is missing, the row is not the expected tool type, or it is not in
/// `session_id`, returns an empty history.
pub fn load_nested_branch_history(
    conn: &mut PooledConn,
    session_id: Uuid,
    anchor_tool_message_id: Option<Uuid>,
    expected_anchor_type: ChatMessageType,
    warn_label: &'static str,
    synthetic_first_user_text: Option<String>,
) -> Result<SessionHistoryLoad, Error> {
    let Some(anchor) = anchor_tool_message_id else {
        return Ok(SessionHistoryLoad {
            session_messages: vec![],
            db_rows_chronological: vec![],
        });
    };

    let synthetic_prefix = synthetic_first_user_text.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        Some((
            ChatSessionRoleType::User,
            ChatMessageType::Text,
            Some(serde_json::json!({ "text": t })),
        ))
    });

    let db_rows_chronological = load_nested_subtree_descendant_rows(
        conn,
        session_id,
        anchor,
        expected_anchor_type,
        warn_label,
        HISTORY_LIMIT as usize,
    )?;

    let mut session_messages: Vec<_> = db_rows_chronological
        .iter()
        .map(|row| (row.role, row.type_, row.payload.clone()))
        .collect();

    if let Some(prefix) = synthetic_prefix {
        session_messages.insert(0, prefix);
        while session_messages.len() > HISTORY_LIMIT as usize {
            if session_messages.len() <= 1 {
                break;
            }
            session_messages.remove(1);
        }
    }

    Ok(SessionHistoryLoad {
        session_messages,
        db_rows_chronological,
    })
}
