//! AI (assistant) message creation and publishing for the chat worker. Used by the LLM layer to
//! create the placeholder message when streaming starts, or the full message when tool-calls-only.

use crate::database::{block_on_db_tokio, BlockingDbError, DbPool};
use crate::models::chat_messages::{
    ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
    TextPayload,
};
use crate::routes::ws::events::{ChatSessionMessageCreatedPayload, WsServerEvent};
use crate::routes::ws::{publish_event_to_channel, user_channel};
use crate::schema::chat_messages;
use diesel::prelude::*;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

/// Ensures the AI response message row exists (inserts if missing) and publishes
/// ChatSessionMessageCreated to Redis. Call when the first streaming content chunk arrives.
pub async fn ensure_ai_message_created_and_publish(
    pool: &DbPool,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    session_id: Uuid,
    response_message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let inserted_row = block_on_db_tokio(pool, move |conn| {
        let row = diesel::insert_into(chat_messages::table)
            .values((
                chat_messages::id.eq(response_message_id),
                chat_messages::chat_session_id.eq(session_id),
                chat_messages::role.eq(ChatSessionRoleType::Ai),
                chat_messages::type_.eq(ChatMessageType::Text),
                chat_messages::payload.eq(None::<serde_json::Value>),
                chat_messages::status.eq(ChatMessageStatus::Generating),
            ))
            .on_conflict(chat_messages::id)
            .do_nothing()
            .returning(ChatMessageRow::as_returning())
            .get_result(conn);
        match row {
            Ok(r) => Ok(Some(r)),
            Err(diesel::result::Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    if let Some(row) = inserted_row {
        let channel = user_channel(user_id);
        let created_message = ChatMessageResponse::from(row);
        let event = WsServerEvent::ChatSessionMessageCreated(ChatSessionMessageCreatedPayload {
            project_id,
            session_id,
            message: created_message,
        });
        let mut guard = redis_conn.lock().await;
        publish_event_to_channel(&mut *guard, &channel, &event).await?;
    }
    Ok(())
}

/// Returns true when the payload has any requested tool calls; status must stay Generating until all tool messages complete (even if there is text content).
fn has_requested_tool_calls(text_payload: &TextPayload) -> bool {
    text_payload
        .requested_tool_calls
        .as_ref()
        .map_or(false, |c| !c.is_empty())
}

/// Creates the AI response message row with the given payload (e.g. tool-calls-only response)
/// and publishes ChatSessionMessageCreated to Redis. Call this before executing tool calls
/// so the client sees the AI message before tool-result messages.
/// When the payload has any requested_tool_calls, the message is saved with status Generating; it will be
/// set to Completed or Failed once all invoked tool messages have finished.
pub async fn create_ai_message_with_payload_and_publish(
    pool: &DbPool,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    session_id: Uuid,
    response_message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
    text_payload: &TextPayload,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let payload_value = serde_json::to_value(text_payload).map_err(|e| e.to_string())?;
    let initial_status = if has_requested_tool_calls(text_payload) {
        ChatMessageStatus::Generating
    } else {
        ChatMessageStatus::Completed
    };
    let inserted_row = block_on_db_tokio(pool, move |conn| {
        diesel::insert_into(chat_messages::table)
            .values((
                chat_messages::id.eq(response_message_id),
                chat_messages::chat_session_id.eq(session_id),
                chat_messages::role.eq(ChatSessionRoleType::Ai),
                chat_messages::type_.eq(ChatMessageType::Text),
                chat_messages::payload.eq(Some(payload_value)),
                chat_messages::status.eq(initial_status),
            ))
            .on_conflict(chat_messages::id)
            .do_nothing()
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
    })
    .await;

    match inserted_row {
        Ok(row) => {
            let channel = user_channel(user_id);
            let created_message = ChatMessageResponse::from(row);
            let event = WsServerEvent::ChatSessionMessageCreated(ChatSessionMessageCreatedPayload {
                project_id,
                session_id,
                message: created_message,
            });
            let mut guard = redis_conn.lock().await;
            publish_event_to_channel(&mut *guard, &channel, &event).await?;
            info!(
                %project_id,
                %session_id,
                %response_message_id,
                "Created AI message (tool-calls-only) and notified client"
            );
        }
        Err(BlockingDbError::Query(diesel::result::Error::NotFound))
        | Err(BlockingDbError::Query(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ))) => {
            // Row already exists (e.g. created when first chunk arrived); update with payload.
            let payload_value = serde_json::to_value(text_payload).map_err(|e| e.to_string())?;
            let update_status = if has_requested_tool_calls(text_payload) {
                ChatMessageStatus::Generating
            } else {
                ChatMessageStatus::Completed
            };
            block_on_db_tokio(pool, move |conn| {
                diesel::update(
                    chat_messages::table
                        .filter(chat_messages::id.eq(response_message_id))
                        .filter(chat_messages::status.ne(ChatMessageStatus::Stopped)),
                )
                .set((
                    chat_messages::payload.eq(Some(payload_value)),
                    chat_messages::status.eq(update_status),
                ))
                .execute(conn)
            })
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
