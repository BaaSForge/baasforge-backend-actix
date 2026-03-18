use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::{ChatAgentType, ChatMessageStatus, ChatMessageType};
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::db::load_previous_session_messages;
use crate::services::chat_worker::llm::{GenerateOutcome, LlmClient};
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
    agent: ChatAgentType,
) -> Result<GenerateOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let session_messages = block_on_db_tokio(pool, move |conn| {
        load_previous_session_messages(conn, session_id, 50)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    let channel = user_channel(user_id);
    let tool_ctx = tool_context_for_chat_session(
        rabbit_publisher.clone(),
        user_id,
        project_id,
        session_id,
        placeholder_ai_message_id,
        agent.clone(),
    );
    let outcome = with_tool_context(tool_ctx, llm_client.generate(
        pool,
        &session_messages,
        user_text,
        Some(redis_conn.clone()),
        &channel,
        placeholder_ai_message_id,
        user_id,
        project_id,
        session_id,
        agent,
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
            Ok(GenerateOutcome::Stopped(text_payload))
        }
        GenerateOutcome::Completed(text_payload) => {
            let has_content = !text_payload.text.is_empty();
            let has_tool_calls = text_payload.requested_tool_calls.as_ref().map_or(false, |c| !c.is_empty());
            if has_content || has_tool_calls {
                let payload = Some(serde_json::to_value(&text_payload)?);
                block_on_db_tokio(pool, move |conn| {
                    let base = diesel::update(
                        chat_messages::table
                            .filter(chat_messages::id.eq(placeholder_ai_message_id))
                            .filter(chat_messages::status.ne(ChatMessageStatus::Stopped)),
                    );
                    // When there are requested tool calls, leave status as Generating until all tool messages complete.
                    if has_tool_calls {
                        base.set(chat_messages::payload.eq(payload)).execute(conn)
                    } else {
                        base.set((
                            chat_messages::payload.eq(payload),
                            chat_messages::status.eq(ChatMessageStatus::Completed),
                        ))
                        .execute(conn)
                    }
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                info!(%project_id, %session_id, "Processed and saved AI response");
            }
            Ok(GenerateOutcome::Completed(text_payload))
        }
    }
}

