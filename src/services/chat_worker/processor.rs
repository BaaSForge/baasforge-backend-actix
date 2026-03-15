use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::ChatMessageStatus;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_previous_session_messages;
use crate::services::chat_worker::llm::{GenerateOutcome, LlmClient, OnFirstChunkFn};
use crate::services::chat_worker::llm::tools::{tool_context_for_chat_session, with_tool_context};
use crate::services::chat_worker::runner;
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
            runner::ensure_ai_message_created_and_publish(
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
        GenerateOutcome::Stopped(partial) => {
            if !partial.is_empty() {
                let payload = Some(serde_json::to_value(&partial)?);
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
        GenerateOutcome::Completed(ai_text) => {
            if !ai_text.is_empty() {
                let payload = Some(serde_json::to_value(&ai_text)?);
                block_on_db_tokio(pool, move |conn| {
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
                info!(%project_id, %session_id, "Processed and saved AI response");
            }
        }
    }
    Ok(())
}

