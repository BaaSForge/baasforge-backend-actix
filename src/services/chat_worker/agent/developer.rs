use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
};
use crate::schema::chat_messages;
use crate::services::chat_worker::units_definitions;
use diesel::prelude::*;
use diesel::result::Error;
use serde_json::Value;
use std::sync::LazyLock;
use uuid::Uuid;

const FETCH_LIMIT: i64 = 50;
const HISTORY_LIMIT: i64 = 50;

pub fn tool_names() -> &'static [&'static str] {
    &[]
}

pub fn load_history(
    conn: &mut PooledConn,
    session_id: Uuid,
    _thread_anchor_message_id: Option<Uuid>,
) -> Result<Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>, Error> {
    let rows = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .order(chat_messages::id.desc())
        .filter(chat_messages::status.eq(ChatMessageStatus::Completed))
        .limit(FETCH_LIMIT)
        .select(ChatMessageRow::as_select())
        .load::<ChatMessageRow>(conn)?;

    let filtered: Vec<_> = rows
        .into_iter()
        .filter(|row| match row.role {
            ChatSessionRoleType::User => true,
            ChatSessionRoleType::Ai => {
                row.agent.as_ref() == Some(&ChatAgentType::Developer) || row.agent.is_none()
            }
        })
        .take(HISTORY_LIMIT as usize)
        .map(|row| (row.role, row.type_, row.payload))
        .collect();

    let out: Vec<_> = filtered.into_iter().rev().collect();
    Ok(out)
}

const SYSTEM_PROMPT_CORE: &str =
    "You are the Developer agent. You implement and refine units and code.
    
Implementation Strategy: To achieve the user's goal, you may modify existing units or build independent units from scratch.
- Priority: Whenever possible, avoid modifying existing units. Prefer creating new units to prevent breaking changes.
- Modifications: If modifying an existing unit is the only viable option, apply changes without breaking current functionality.
- Breaking Changes: This is the absolute last resort, considered only if no other path exists.
";

pub static SYSTEM_PROMPT_WITH_UNIT_DETAILS: LazyLock<&'static str> = LazyLock::new(|| {
    let combined = format!(
        "{SYSTEM_PROMPT_CORE}\n\n## Unit type reference (detailed)\n\nUse the following when implementing or modifying units so behavior stays consistent with platform unit semantics.\n\n{}",
        units_definitions::all_details(),
    );
    Box::leak(combined.into_boxed_str())
});
