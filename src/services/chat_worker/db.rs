use crate::database::PooledConn;
use crate::models::chat_messages::{ChatMessageRow, ChatMessageStatus, ChatSessionRoleType};
use crate::schema::chat_messages;
use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

pub fn load_previous_session_messages(
    conn: &mut PooledConn,
    session_id: Uuid,
    limit: i64,
) -> Result<Vec<(ChatSessionRoleType, Option<Value>)>, diesel::result::Error> {
    let rows = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .order(chat_messages::id.asc())
        .filter(chat_messages::status.eq(ChatMessageStatus::Completed))
        .limit(limit)
        .select(ChatMessageRow::as_select())
        .load::<ChatMessageRow>(conn)?;

    let out = rows
        .into_iter()
        .map(|row| (row.role, row.payload))
        .collect();
    Ok(out)
}

