use crate::database::{DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, TextPayload,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::{publish_event_to_channel, user_channel};
use crate::schema::chat_messages;
use crate::services::chat_worker::agent::load_history_for_agent;
use crate::services::chat_worker::llm::tools::{tool_context_for_chat_session, with_tool_context};
use crate::services::releases::chat_session_is_read_only_for_releases;
use crate::services::chat_worker::llm::{GenerateOutcome, LlmClient};
use diesel::prelude::*;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

fn text_payload_has_storable_content(p: &TextPayload) -> bool {
    !p.text.is_empty()
        || p.requested_tool_calls
            .as_ref()
            .is_some_and(|c| !c.is_empty())
        || p.has_thinking_stored()
}

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
    // Same as `ChatSessionQueueMessage::invoking_message_id`: parent for the new AI Text row (None = main
    // thread; Some = nested under that id). For [`ChatAgentType::ArchitectureAnalyzer`],
    // [`ChatAgentType::SolutionAnalyzer`], and [`ChatAgentType::Developer`] that parent is the corresponding
    // async tool row id — used as history root for nested-only context.
    queue_invoking_message_id: Option<Uuid>,
) -> Result<GenerateOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let agent_for_history = agent.clone();
    let thread_anchor = queue_invoking_message_id;
    let session_messages = block_on_db_tokio(pool, move |conn| {
        load_history_for_agent(conn, session_id, agent_for_history, thread_anchor)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    let session_read_only = block_on_db_tokio(pool, move |conn| {
        chat_session_is_read_only_for_releases(conn, project_id, session_id)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    let channel = user_channel(user_id);
    // Tool rows published from this LLM pass must reference `placeholder_ai_message_id` (this AI Text
    // message), whether the agent is Planner or a nested sub-agent — so nested tool calls chain
    // under the correct parent message.
    let tool_ctx = tool_context_for_chat_session(
        rabbit_publisher.clone(),
        user_id,
        project_id,
        session_id,
        placeholder_ai_message_id,
        agent.clone(),
        session_read_only,
    );
    let outcome = with_tool_context(
        tool_ctx,
        llm_client.generate(
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
            queue_invoking_message_id,
            session_read_only,
        ),
    )
    .await?;

    match outcome {
        GenerateOutcome::Stopped(text_payload) => {
            if text_payload_has_storable_content(&text_payload) {
                let payload = Some(serde_json::to_value(&text_payload)?);
                block_on_db_tokio(pool, move |conn| {
                    diesel::update(
                        chat_messages::table
                            .filter(chat_messages::id.eq(placeholder_ai_message_id)),
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
            let has_tool_calls = text_payload
                .requested_tool_calls
                .as_ref()
                .is_some_and(|c| !c.is_empty());
            let has_thinking = text_payload.has_thinking_stored();
            if has_content || has_tool_calls || has_thinking {
                let payload = Some(serde_json::to_value(&text_payload)?);
                let placeholder_id = placeholder_ai_message_id;
                let updated_row = block_on_db_tokio(pool, move |conn| {
                    let q = chat_messages::table
                        .filter(chat_messages::id.eq(placeholder_id))
                        .filter(chat_messages::status.ne(ChatMessageStatus::Stopped));
                    // When there are requested tool calls, leave status as Generating until all tool messages complete.
                    if has_tool_calls {
                        diesel::update(q)
                            .set(chat_messages::payload.eq(payload))
                            .returning(ChatMessageRow::as_returning())
                            .get_result(conn)
                            .optional()
                    } else {
                        diesel::update(q)
                            .set((
                                chat_messages::payload.eq(payload),
                                chat_messages::status.eq(ChatMessageStatus::Completed),
                            ))
                            .returning(ChatMessageRow::as_returning())
                            .get_result(conn)
                            .optional()
                    }
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                if let Some(row) = updated_row {
                    let message = ChatMessageResponse::from(row);
                    let event =
                        WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
                            project_id,
                            session_id,
                            message,
                        });
                    let mut guard = redis_conn.lock().await;
                    publish_event_to_channel(&mut *guard, &channel, &event)
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                            e.to_string().into()
                        })?;
                }
                info!(%project_id, %session_id, "Processed and saved AI response");
            }
            Ok(GenerateOutcome::Completed(text_payload))
        }
    }
}
