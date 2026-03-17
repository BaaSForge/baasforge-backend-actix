use crate::database::{block_on_db_tokio, BlockingDbError, DbPool};
use crate::models::chat_messages::{
    ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
    TextPayload,
};
use crate::routes::ws::events::{ChatSessionMessageCreatedPayload, WsServerEvent};
use crate::routes::ws::publish_event_to_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_previous_session_messages;
use crate::services::chat_worker::llm::{GenerateOutcome, LlmClient, OnFirstChunkFn};
use crate::services::chat_worker::llm::tools::{tool_context_for_chat_session, with_tool_context};
use diesel::prelude::*;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

pub async fn process_one(
    pool: &DbPool,
    llm_client: &LlmClient,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    rabbit_publisher: &Arc<crate::services::message_queue::SharedRabbitPublisher>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    user_text: &str,
    placeholder_ai_message_id: Uuid,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session_messages = block_on_db_tokio(pool, move |conn| {
        load_previous_session_messages(conn, session_id, 50)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
    let pool_clone = pool.clone();
    let redis_conn_clone = redis_conn.clone();
    let on_first_content_chunk = Some(OnFirstChunkFn::new(move || {
        let pool = pool_clone.clone();
        let redis = redis_conn_clone.clone();
        Box::pin(async move {
            ensure_ai_message_created_and_publish(
                &pool,
                &redis,
                session_id,
                placeholder_ai_message_id,
                user_id,
                project_id,
            )
            .await
        })
    }));
    let tool_ctx = tool_context_for_chat_session(
        rabbit_publisher.clone(),
        user_id,
        project_id,
        session_id,
    );
    let outcome = with_tool_context(tool_ctx, llm_client.generate(
        &session_messages,
        user_text,
        Some(redis_conn.clone()),
        &topic,
        placeholder_ai_message_id,
        on_first_content_chunk,
    ))
    .await?;

    match outcome {
        GenerateOutcome::Stopped(text_payload) => {
            if !text_payload.text.is_empty() || text_payload.requested_tool_calls.as_ref().map_or(false, |c| !c.is_empty()) {
                let payload = Some(serde_json::to_value(&text_payload)?);
                block_on_db_tokio(pool, move |conn| {
                    diesel::update(
                        chat_messages::table.filter(chat_messages::id.eq(placeholder_ai_message_id)),
                    )
                    .set((
                        chat_messages::payload.eq(payload),
                        chat_messages::status.eq(ChatMessageStatus::Stopped),
                    ))
                    .execute(conn)
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                info!(
                    %project_id,
                    %session_id,
                    %placeholder_ai_message_id,
                    "Streaming stopped by user, saved partial content to DB"
                );
            }
        }
        GenerateOutcome::Completed(text_payload) => {
            if !text_payload.text.is_empty() || text_payload.requested_tool_calls.as_ref().map_or(false, |c| !c.is_empty()) {
                let payload = Some(serde_json::to_value(&text_payload)?);
                let rows_affected = block_on_db_tokio(pool, move |conn| {
                    diesel::update(
                        chat_messages::table
                            .filter(chat_messages::id.eq(placeholder_ai_message_id))
                            .filter(chat_messages::status.ne(ChatMessageStatus::Stopped)),
                    )
                    .set((
                        chat_messages::payload.eq(payload),
                        chat_messages::status.eq(ChatMessageStatus::Completed),
                    ))
                    .execute(conn)
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

                // When LLM returned only tool calls (no text), on_first_content_chunk was never
                // called, so the message row doesn't exist. Create it and notify the client.
                if rows_affected == 0 {
                    create_ai_message_with_payload_and_publish(
                        pool,
                        redis_conn,
                        session_id,
                        placeholder_ai_message_id,
                        user_id,
                        project_id,
                        &text_payload,
                    )
                    .await?;
                } else {
                    info!(%project_id, %session_id, "Processed and saved AI response");
                }
            }
        }
    }
    Ok(())
}

/// Ensures the AI response message row exists (inserts if missing) and publishes
/// ChatSessionMessageCreated to Redis. Used when the first streaming chunk arrives
/// so we avoid creating empty messages when the AI only calls tools.
async fn ensure_ai_message_created_and_publish(
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
        let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
        let created_message = ChatMessageResponse::from(row);
        let event = WsServerEvent::ChatSessionMessageCreated(ChatSessionMessageCreatedPayload {
            message: created_message,
        });
        let mut guard = redis_conn.lock().await;
        publish_event_to_channel(&mut *guard, &topic, &event).await?;
    }
    Ok(())
}

/// Creates the AI response message row with the given payload (e.g. tool-calls-only response)
/// and publishes ChatSessionMessageCreated to Redis. Use when the LLM returned tool calls
/// but no text, so the message was never created via on_first_content_chunk.
async fn create_ai_message_with_payload_and_publish(
    pool: &DbPool,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    session_id: Uuid,
    response_message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
    text_payload: &TextPayload,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let payload_value = serde_json::to_value(text_payload).map_err(|e| e.to_string())?;
    let inserted_row = block_on_db_tokio(pool, move |conn| {
        diesel::insert_into(chat_messages::table)
            .values((
                chat_messages::id.eq(response_message_id),
                chat_messages::chat_session_id.eq(session_id),
                chat_messages::role.eq(ChatSessionRoleType::Ai),
                chat_messages::type_.eq(ChatMessageType::Text),
                chat_messages::payload.eq(Some(payload_value)),
                chat_messages::status.eq(ChatMessageStatus::Completed),
            ))
            .on_conflict(chat_messages::id)
            .do_nothing()
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
    })
    .await;

    match inserted_row {
        Ok(row) => {
            let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
            let created_message = ChatMessageResponse::from(row);
            let event = WsServerEvent::ChatSessionMessageCreated(ChatSessionMessageCreatedPayload {
                message: created_message,
            });
            let mut guard = redis_conn.lock().await;
            publish_event_to_channel(&mut *guard, &topic, &event).await?;
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
            // Row already exists (e.g. created by on_first_content_chunk); update with payload.
            let payload_value = serde_json::to_value(text_payload).map_err(|e| e.to_string())?;
            block_on_db_tokio(pool, move |conn| {
                diesel::update(
                    chat_messages::table
                        .filter(chat_messages::id.eq(response_message_id))
                        .filter(chat_messages::status.ne(ChatMessageStatus::Stopped)),
                )
                .set((
                    chat_messages::payload.eq(Some(payload_value)),
                    chat_messages::status.eq(ChatMessageStatus::Completed),
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
