//! Single place that knows when the generation loop (streaming + tool calling) is over
//! and notifies the client via WebSocket.
//!
//! Generation ends when no continuation message will be scheduled. The only code that
//! ever schedules a continuation is `maybe_complete_invoking_message` (after tool messages
//! complete). So "no continuation" is known in exactly one place: the runner, after
//! `process_one` returns, when the outcome has no pending tool calls. If there were
//! tool calls, tool messages run and eventually a continuation is published; the loop
//! then continues until a later `process_one` returns with no tool calls.

use crate::routes::ws::events::{ChatSessionGenerationEndedPayload, WsServerEvent};
use crate::routes::ws::redis::{publish_event_to_channel, user_channel};
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Called from the runner only, after `process_one` for a TEXT message. If the outcome
/// has no pending tool calls, no continuation will ever be scheduled (generation loop is
/// over), so we broadcast `ChatSessionGenerationEnded` to the user's channel.
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
