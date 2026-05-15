//! Single place that knows when the generation loop (streaming + tool calling) is over
//! and notifies the client via WebSocket.
//!
//! Generation ends when no continuation message will be scheduled. Continuations are published from
//! `maybe_complete_invoking_message` (after tool messages complete) and from the chat worker runner
//! when a nested sub-agent completes with text but no tool calls (stuck-run nudge). "No continuation"
//! is decided in the runner after `process_one`: if neither path applies, the loop is over.

use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageStatus, ChatMessageType, ChatSessionRoleType, TextPayload,
};
use crate::routes::ws::events::{
    ChatSessionGenerationEndedPayload, ChatSessionMessageStatusChangePayload, WsServerEvent,
};
use crate::routes::ws::redis::{publish_event_to_channel, user_channel};
use crate::schema::chat_messages;
use crate::services::chat_worker::llm::tools::cascade_sub_agent_ai_text_failure_to_planner;
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub async fn fail_generating_placeholder_message(
    pool: &DbPool,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    rabbit_publisher: &Arc<SharedRabbitPublisher>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    response_message_id: Uuid,
    agent: ChatAgentType,
    user_visible_text: Option<&str>,
    cascade_error: &str,
    notify_generation_loop: bool,
) -> bool {
    let message_id = response_message_id;
    let session_id_db = session_id;
    let agent_db = agent.clone();
    let payload_val = user_visible_text.and_then(|t| {
        let text = t.trim();
        if text.is_empty() {
            return None;
        }
        serde_json::to_value(TextPayload {
            text: text.to_string(),
            requested_tool_calls: None,
            thinking_blocks: None,
        })
        .ok()
    });
    let db_ok = match payload_val {
        Some(pv) => {
            let pv2 = pv.clone();
            block_on_db_tokio(pool, move |conn| {
                diesel::insert_into(chat_messages::table)
                    .values((
                        chat_messages::id.eq(message_id),
                        chat_messages::chat_session_id.eq(session_id_db),
                        chat_messages::role.eq(ChatSessionRoleType::Ai),
                        chat_messages::type_.eq(ChatMessageType::Text),
                        chat_messages::payload.eq(Some(pv)),
                        chat_messages::status.eq(ChatMessageStatus::Failed),
                        chat_messages::agent.eq(Some(agent_db)),
                    ))
                    .on_conflict(chat_messages::id)
                    .do_update()
                    .set((
                        chat_messages::status.eq(ChatMessageStatus::Failed),
                        chat_messages::payload.eq(Some(pv2)),
                    ))
                    .execute(conn)
            })
            .await
            .is_ok()
        }
        None => {
            block_on_db_tokio(pool, move |conn| {
                diesel::insert_into(chat_messages::table)
                    .values((
                        chat_messages::id.eq(message_id),
                        chat_messages::chat_session_id.eq(session_id_db),
                        chat_messages::role.eq(ChatSessionRoleType::Ai),
                        chat_messages::type_.eq(ChatMessageType::Text),
                        chat_messages::payload.eq(None::<serde_json::Value>),
                        chat_messages::status.eq(ChatMessageStatus::Failed),
                        chat_messages::agent.eq(Some(agent_db)),
                    ))
                    .on_conflict(chat_messages::id)
                    .do_update()
                    .set(chat_messages::status.eq(ChatMessageStatus::Failed))
                    .execute(conn)
            })
            .await
            .is_ok()
        }
    };

    if !db_ok {
        tracing::error!(
            %response_message_id,
            "fail_generating_placeholder_message: upsert Failed in DB failed"
        );
        return false;
    }

    let cascaded_planner_continuation = cascade_sub_agent_ai_text_failure_to_planner(
        pool,
        redis_conn,
        Some(rabbit_publisher),
        user_id,
        project_id,
        session_id,
        response_message_id,
        agent,
        cascade_error.to_string(),
    )
    .await;
    if notify_generation_loop {
        notify_if_generation_loop_finished(
            redis_conn,
            user_id,
            project_id,
            session_id,
            cascaded_planner_continuation,
        )
        .await;
    }
    let channel = user_channel(user_id);
    let event = WsServerEvent::ChatSessionMessageStatusChange(ChatSessionMessageStatusChangePayload {
        project_id,
        session_id,
        message_id: response_message_id,
        status: ChatMessageStatus::Failed,
    });
    let mut guard = redis_conn.lock().await;
    if let Err(redis_e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
        tracing::warn!(
            "fail_generating_placeholder_message: Redis status event failed: {}",
            redis_e
        );
    }
    cascaded_planner_continuation
}

/// Called from the runner only, after `process_one` for a TEXT message. When `will_schedule_continuation`
/// is false (no tool calls and no stuck-sub-agent re-queue), broadcast `ChatSessionGenerationEnded`.
pub async fn notify_if_generation_loop_finished(
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    will_schedule_continuation: bool,
) {
    if will_schedule_continuation {
        return;
    }
    let channel = user_channel(user_id);
    let event = WsServerEvent::ChatSessionGenerationEnded(ChatSessionGenerationEndedPayload {
        project_id,
        session_id,
    });
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
        tracing::warn!("Failed to broadcast generation ended to Redis: {}", e);
    }
}
