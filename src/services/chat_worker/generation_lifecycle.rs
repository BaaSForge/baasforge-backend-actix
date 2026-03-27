//! Single place that knows when the generation loop (streaming + tool calling) is over
//! and notifies the client via WebSocket.
//!
//! Generation ends when no continuation message will be scheduled. Continuations are published from
//! `maybe_complete_invoking_message` (after tool messages complete) and from the chat worker runner
//! when a nested sub-agent completes with text but no tool calls (stuck-run nudge). "No continuation"
//! is decided in the runner after `process_one`: if neither path applies, the loop is over.

use crate::routes::ws::events::{ChatSessionGenerationEndedPayload, WsServerEvent};
use crate::routes::ws::redis::{publish_event_to_channel, user_channel};
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

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
