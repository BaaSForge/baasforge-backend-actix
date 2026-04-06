use crate::database::{block_on_db_tokio, DbPool};
use crate::models::chat_messages::{ChatMessageResponse, ChatMessageRow, ChatMessageStatus};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::llm::tools::create_unit::ToolCreateUnitPayload;
use crate::services::chat_worker::llm::tools::modify_unit::ToolModifyUnitPayload;
use crate::services::chat_worker::llm::tools::remove_unit::ToolRemoveUnitPayload;
use crate::services::chat_worker::llm::tools::{maybe_complete_invoking_message, UnitSubTaskStatus};
use crate::services::message_queue::SharedRabbitPublisher;
use crate::services::unit_technical_reviewer::queue::{
    UnitReviewerMessageType, UnitReviewerQueueMessage,
};
use diesel::prelude::*;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type RedisConn = Arc<Mutex<MultiplexedConnection>>;

/// Re-exported for convenience so callers can import from `helpers` without knowing the module
/// structure changed.
pub use crate::services::unit_technical_reviewer::rule_checker::{run_rule_check, UnitOperation};

pub async fn load_tool_message_row(
    pool: &DbPool,
    tool_message_id: Uuid,
) -> Result<ChatMessageRow, BoxError> {
    block_on_db_tokio(pool, move |conn| {
        chat_messages::table
            .filter(chat_messages::id.eq(tool_message_id))
            .select(ChatMessageRow::as_select())
            .first::<ChatMessageRow>(conn)
    })
    .await
    .map_err(|e| -> BoxError { e.to_string().into() })
}

pub fn extract_create_unit_payload(row: &ChatMessageRow) -> Result<ToolCreateUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("create_unit tool message has no payload")?;
    serde_json::from_value::<ToolCreateUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolCreateUnitPayload: {e}").into() })
}

pub fn extract_modify_unit_payload(row: &ChatMessageRow) -> Result<ToolModifyUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("modify_unit tool message has no payload")?;
    serde_json::from_value::<ToolModifyUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolModifyUnitPayload: {e}").into() })
}

pub fn extract_remove_unit_payload(row: &ChatMessageRow) -> Result<ToolRemoveUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("remove_unit tool message has no payload")?;
    serde_json::from_value::<ToolRemoveUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolRemoveUnitPayload: {e}").into() })
}

pub async fn update_tool_message_payload<P: serde::Serialize>(
    pool: &DbPool,
    tool_message_id: Uuid,
    payload: &P,
    status: ChatMessageStatus,
) -> Result<ChatMessageRow, BoxError> {
    let payload_value = serde_json::to_value(payload)
        .map_err(|e| -> BoxError { format!("serialize reviewer payload: {e}").into() })?;
    block_on_db_tokio(pool, move |conn| {
        diesel::update(chat_messages::table.filter(chat_messages::id.eq(tool_message_id)))
            .set((
                chat_messages::payload.eq(Some(payload_value)),
                chat_messages::status.eq(status),
            ))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
    })
    .await
    .map_err(|e| -> BoxError { e.to_string().into() })
}

pub async fn broadcast_message_updated(
    redis_conn: &RedisConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    row: ChatMessageRow,
) {
    let channel = user_channel(user_id);
    let message: ChatMessageResponse = row.into();
    let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
        project_id,
        session_id,
        message,
    });
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
        tracing::warn!("unit reviewer: failed to broadcast ChatSessionMessageUpdated: {}", e);
    }
}

/// Marks a tool message as `Completed` with all still-`Waiting` sub-tasks set to `Failed`,
/// broadcasts the update, and triggers LLM continuation.
///
/// Called from the runner when a message is dead-lettered after exhausting all retries.
/// Without this the tool message stays `Generating` indefinitely and the LLM never unblocks.
/// Best-effort: if loading or updating the DB row fails we still attempt LLM continuation.
pub async fn fail_tool_message_and_continue(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: &UnitReviewerQueueMessage,
    reason: &str,
) {
    let tool_message_id = message.tool_message_id;
    let row = match load_tool_message_row(pool, tool_message_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                %tool_message_id,
                "fail_tool_message_and_continue: could not load tool message row: {}",
                e
            );
            // Still attempt LLM continuation so it is not stuck on a Generating tool message.
            trigger_llm_continuation(
                pool,
                redis_conn,
                publisher,
                None,
                message.user_id,
                message.project_id,
                message.session_id,
            )
            .await;
            return;
        }
    };
    let invoking_message_id = row.invoking_message_id;

    // Only flip sub-tasks that are still Waiting; leave already-Succeeded ones intact so the
    // LLM sees an accurate picture of how far the checks got before the reviewer crashed.
    let updated_row = match message.message_type {
        UnitReviewerMessageType::CreateUnitCompilerCheck
        | UnitReviewerMessageType::CreateUnitRuleCheck => {
            match extract_create_unit_payload(&row) {
                Ok(mut payload) => {
                    if payload.compiler_check_sub_task == UnitSubTaskStatus::Waiting {
                        payload.compiler_check_sub_task = UnitSubTaskStatus::Failed;
                    }
                    if payload.rule_check_sub_task == UnitSubTaskStatus::Waiting {
                        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
                    }
                    payload.error = Some(reason.to_string());
                    update_tool_message_payload(
                        pool,
                        tool_message_id,
                        &payload,
                        ChatMessageStatus::Completed,
                    )
                    .await
                }
                Err(e) => Err(e),
            }
        }
        UnitReviewerMessageType::ModifyUnitCompilerCheck
        | UnitReviewerMessageType::ModifyUnitRuleCheck => {
            match extract_modify_unit_payload(&row) {
                Ok(mut payload) => {
                    if payload.compiler_check_sub_task == UnitSubTaskStatus::Waiting {
                        payload.compiler_check_sub_task = UnitSubTaskStatus::Failed;
                    }
                    if payload.rule_check_sub_task == UnitSubTaskStatus::Waiting {
                        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
                    }
                    payload.error = Some(reason.to_string());
                    update_tool_message_payload(
                        pool,
                        tool_message_id,
                        &payload,
                        ChatMessageStatus::Completed,
                    )
                    .await
                }
                Err(e) => Err(e),
            }
        }
        UnitReviewerMessageType::RemoveUnitRuleCheck => {
            match extract_remove_unit_payload(&row) {
                Ok(mut payload) => {
                    if payload.rule_check_sub_task == UnitSubTaskStatus::Waiting {
                        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
                    }
                    payload.error = Some(reason.to_string());
                    update_tool_message_payload(
                        pool,
                        tool_message_id,
                        &payload,
                        ChatMessageStatus::Completed,
                    )
                    .await
                }
                Err(e) => Err(e),
            }
        }
    };

    match updated_row {
        Ok(row) => {
            broadcast_message_updated(
                redis_conn,
                message.user_id,
                message.project_id,
                message.session_id,
                row,
            )
            .await;
        }
        Err(e) => {
            tracing::error!(
                %tool_message_id,
                "fail_tool_message_and_continue: could not update tool message: {}",
                e
            );
        }
    }

    trigger_llm_continuation(
        pool,
        redis_conn,
        publisher,
        invoking_message_id,
        message.user_id,
        message.project_id,
        message.session_id,
    )
    .await;
}

/// Calls `maybe_complete_invoking_message` so the AI Text invoker is completed and the LLM
/// receives a queue continuation. Should be called whenever a tool message is set to Completed
/// (success or failure) in the reviewer — in both cases the LLM must be allowed to continue.
pub async fn trigger_llm_continuation(
    pool: &DbPool,
    redis_conn: &RedisConn,
    chat_publisher: &Arc<SharedRabbitPublisher>,
    invoking_message_id: Option<Uuid>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
) {
    let Some(inv_id) = invoking_message_id else {
        tracing::warn!(
            "unit reviewer: invoking_message_id is None; cannot trigger LLM continuation"
        );
        return;
    };
    maybe_complete_invoking_message(
        pool,
        redis_conn,
        Some(chat_publisher),
        inv_id,
        user_id,
        project_id,
        session_id,
        true,
    )
    .await;
}
