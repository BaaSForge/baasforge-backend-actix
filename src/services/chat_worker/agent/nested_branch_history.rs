//! Shared history loading for nested sub-agents whose conversation hangs under an async tool row
//! (e.g. `ToolAnalyzeArchitecture`, `ToolAnalyzeSolution`, `ToolImplementSolution`).
//!
//! These subtrees are **not** visible to the Planner’s other messages or to other agents’ subtrees;
//! each invocation is a separate branch. The Planner must put carry-over context in **intent** /
//! **directive** (see planner system prompt §2.1 and [`crate::services::chat_worker::agent::load_history_for_agent`]).
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
use diesel::prelude::*;
use diesel::result::Error;
use diesel::OptionalExtension;
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

const HISTORY_LIMIT: i64 = 50;

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

/// Loads **completed** history only under the nested branch rooted at the given tool message id.
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
) -> Result<Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>, Error> {
    let Some(anchor) = anchor_tool_message_id else {
        return Ok(vec![]);
    };

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
            "{}::load_history: expected id of nested analyze tool row in session; returning empty history",
            warn_label
        );
        return Ok(vec![]);
    }

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

    let ids = subtree_message_ids(conn, session_id, anchor)?;
    let ids: Vec<Uuid> = ids.into_iter().filter(|id| *id != anchor).collect();
    let rows: Vec<ChatMessageRow> = if ids.is_empty() {
        vec![]
    } else {
        chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(chat_messages::id.eq_any(ids))
            .filter(chat_messages::status.eq(ChatMessageStatus::Completed))
            .order(chat_messages::id.asc())
            .select(ChatMessageRow::as_select())
            .load(conn)?
    };

    let mut mapped: Vec<_> = rows
        .into_iter()
        .map(|row| (row.role, row.type_, row.payload))
        .collect();

    if mapped.len() > HISTORY_LIMIT as usize {
        let start = mapped.len() - HISTORY_LIMIT as usize;
        mapped.drain(..start);
    }

    if let Some(prefix) = synthetic_prefix {
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
