use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::ChatMessageStatus;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_previous_session_messages;
use crate::services::chat_worker::llm::{GenerateOutcome, LlmClient};
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
    let outcome = llm_client
        .generate(
            &session_messages,
            user_text,
            Some(redis_conn.clone()),
            &topic,
            placeholder_ai_message_id,
        )
        .await?;

    match outcome {
        GenerateOutcome::Stopped(partial) => {
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
        GenerateOutcome::Completed(ai_text) => {
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
    Ok(())
}

