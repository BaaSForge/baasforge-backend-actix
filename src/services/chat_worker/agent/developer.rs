use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageRow, ChatMessageStatus, ChatSessionRoleType,
};
use crate::schema::chat_messages;
use diesel::prelude::*;
use diesel::result::Error;
use serde_json::Value;
use uuid::Uuid;

const FETCH_LIMIT: i64 = 50;
const HISTORY_LIMIT: i64 = 50;

pub fn load_history(
    conn: &mut PooledConn,
    session_id: Uuid,
) -> Result<Vec<(ChatSessionRoleType, Option<Value>)>, Error> {
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
        .map(|row| (row.role, row.payload))
        .collect();

    let out: Vec<_> = filtered.into_iter().rev().collect();
    Ok(out)
}

pub const SYSTEM_PROMPT: &str = "You are the Developer agent. You implement and refine units and code.";
