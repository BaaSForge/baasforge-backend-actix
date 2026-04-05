use crate::configs::Settings;
use crate::database::{block_on_db_tokio, establish_pool, DbPool};
use crate::models::chat_messages::{ChatMessageResponse, ChatMessageRow, ChatMessageStatus};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::llm::tools::create_unit::ToolCreateUnitPayload;
use crate::services::chat_worker::llm::tools::modify_unit::ToolModifyUnitPayload;
use crate::services::chat_worker::llm::tools::remove_unit::ToolRemoveUnitPayload;
use crate::services::chat_worker::llm::tools::{maybe_complete_invoking_message, UnitSubTaskStatus};
use crate::services::message_queue::{create_shared_publisher, SharedRabbitPublisher};
use crate::services::unit_technical_reviewer::queue::{
    create_reviewer_work_consumer_on_connection, publish_reviewer_message,
    UnitReviewerMessageType, UnitReviewerQueueMessage, UNIT_REVIEWER_EXCHANGE,
};
use diesel::prelude::*;
use lapin::options::BasicAckOptions;
use lapin::ConnectionProperties;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::info;
use uuid::Uuid;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type RedisConn = Arc<Mutex<MultiplexedConnection>>;

pub async fn run() -> Result<(), BoxError> {
    tracing_subscriber::fmt::init();

    let settings = Settings::new()
        .map_err(|e| -> BoxError { e.to_string().into() })?;

    let redis_client = redis::Client::open(settings.redis_url.as_str())?;
    let redis_conn = redis_client.get_multiplexed_async_connection().await?;
    let redis_conn = Arc::new(Mutex::new(redis_conn));
    info!("Connected to Redis");

    let pool = establish_pool(
        settings.pg_database_url.clone(),
        settings.database_pool_size,
        settings.database_connection_timeout_secs,
    );

    let rabbit_url = settings.rabbit_url.clone();
    info!("Connecting to RabbitMQ...");
    let connection = lapin::Connection::connect(&rabbit_url, ConnectionProperties::default())
        .await
        .map_err(|e| -> BoxError { e.into() })?;
    let (_consumer_channel, mut consumer) =
        create_reviewer_work_consumer_on_connection(&connection)
            .await
            .map_err(|e| -> BoxError { e.into() })?;

    // Single publisher: used both for dispatching back to chat_session_exchange
    // (via maybe_complete_invoking_message → publish_with_publisher which hardcodes CHAT_SESSION_EXCHANGE)
    // and for dispatching next reviewer checks (to UNIT_REVIEWER_EXCHANGE via publish_reviewer_message).
    // publish_with_channel / publish_with_publisher both accept an exchange name at call time.
    let publisher = create_shared_publisher(&rabbit_url, UNIT_REVIEWER_EXCHANGE)
        .await
        .map_err(|e| -> BoxError { e.into() })?;

    info!("Unit technical reviewer started. Consuming from reviewer work queue.");

    while let Some(delivery_result) = consumer.next().await {
        let delivery = match delivery_result {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Consumer error: {}", e);
                return Err(std::io::Error::other(format!("consumer error: {e}")).into());
            }
        };

        let message = match serde_json::from_slice::<UnitReviewerQueueMessage>(&delivery.data) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Invalid reviewer message body: {}", e);
                let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                continue;
            }
        };

        tracing::info!(
            tool_message_id = %message.tool_message_id,
            message_type = ?message.message_type,
            "Unit reviewer: processing message"
        );

        let result = process_message(&pool, &redis_conn, &publisher, message).await;
        if let Err(e) = result {
            tracing::error!("Unit reviewer message processing failed: {}", e);
        }

        if delivery.acker.ack(BasicAckOptions::default()).await.is_err() {
            tracing::warn!("Failed to ack delivery");
        }
    }

    Ok(())
}

async fn process_message(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    match message.message_type {
        UnitReviewerMessageType::CreateUnitCompilerCheck => {
            process_create_unit_compiler_check(pool, redis_conn, publisher, message).await
        }
        UnitReviewerMessageType::CreateUnitRuleCheck => {
            process_create_unit_rule_check(pool, redis_conn, publisher, message).await
        }
        UnitReviewerMessageType::ModifyUnitCompilerCheck => {
            process_modify_unit_compiler_check(pool, redis_conn, publisher, message).await
        }
        UnitReviewerMessageType::ModifyUnitRuleCheck => {
            process_modify_unit_rule_check(pool, redis_conn, publisher, message).await
        }
        UnitReviewerMessageType::RemoveUnitRuleCheck => {
            process_remove_unit_rule_check(pool, redis_conn, publisher, message).await
        }
    }
}

// ── create_unit: compiler check ─────────────────────────────────────────────

async fn process_create_unit_compiler_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_create_unit_payload(&row)?;

    // Simulate compiler check — code inspection will be added later.
    let check_succeeded = simulate_compiler_check(&message.code);

    if check_succeeded {
        payload.compiler_check_sub_task = UnitSubTaskStatus::Succeeded;
        // Status stays Generating; rule check follows.
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Generating)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
        // Dispatch rule check as the next step.
        let next = UnitReviewerQueueMessage {
            message_type: UnitReviewerMessageType::CreateUnitRuleCheck,
            ..message
        };
        publish_reviewer_message(publisher, &next)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
    } else {
        let error_msg = "Compiler check failed: code contains syntax errors.".to_string();
        payload.compiler_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some(error_msg);
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
        // Done (failed): let the LLM continue so it can react to the failure.
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

    Ok(())
}

// ── create_unit: rule check ──────────────────────────────────────────────────

async fn process_create_unit_rule_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_create_unit_payload(&row)?;

    // Simulate rule check — linting / naming-convention checks will be added later.
    let check_succeeded = simulate_rule_check(&message.unit_type, &message.context_slug);

    if check_succeeded {
        payload.rule_check_sub_task = UnitSubTaskStatus::Succeeded;
    } else {
        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some("Rule check failed: unit violates naming conventions.".to_string());
    }

    // Rule check is always the last step for create_unit: mark Completed regardless.
    let updated_row =
        update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
            .await?;
    broadcast_message_updated(
        redis_conn,
        message.user_id,
        message.project_id,
        message.session_id,
        updated_row,
    )
    .await;

    // Let the LLM continue in both success and failure cases.
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

    Ok(())
}

// ── modify_unit: compiler check ──────────────────────────────────────────────

async fn process_modify_unit_compiler_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_modify_unit_payload(&row)?;

    let check_succeeded = simulate_compiler_check(&message.code);

    if check_succeeded {
        payload.compiler_check_sub_task = UnitSubTaskStatus::Succeeded;
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Generating)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
        let next = UnitReviewerQueueMessage {
            message_type: UnitReviewerMessageType::ModifyUnitRuleCheck,
            ..message
        };
        publish_reviewer_message(publisher, &next)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
    } else {
        payload.compiler_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some("Compiler check failed: code contains syntax errors.".to_string());
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
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

    Ok(())
}

// ── modify_unit: rule check ──────────────────────────────────────────────────

async fn process_modify_unit_rule_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_modify_unit_payload(&row)?;

    let check_succeeded = simulate_rule_check(&message.unit_type, &message.context_slug);

    if check_succeeded {
        payload.rule_check_sub_task = UnitSubTaskStatus::Succeeded;
    } else {
        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some("Rule check failed: unit violates naming conventions.".to_string());
    }

    let updated_row =
        update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
            .await?;
    broadcast_message_updated(
        redis_conn,
        message.user_id,
        message.project_id,
        message.session_id,
        updated_row,
    )
    .await;
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

    Ok(())
}

// ── remove_unit: rule check ──────────────────────────────────────────────────

async fn process_remove_unit_rule_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_remove_unit_payload(&row)?;

    let check_succeeded = simulate_rule_check(&message.unit_type, &message.context_slug);

    if check_succeeded {
        payload.rule_check_sub_task = UnitSubTaskStatus::Succeeded;
    } else {
        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some("Rule check failed: cannot remove this unit type.".to_string());
    }

    let updated_row =
        update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
            .await?;
    broadcast_message_updated(
        redis_conn,
        message.user_id,
        message.project_id,
        message.session_id,
        updated_row,
    )
    .await;
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

    Ok(())
}

// ── shared helpers ───────────────────────────────────────────────────────────

/// Simulates a compiler/syntax check on JavaScript code.
/// For now always succeeds; real AST/parse checks will replace this.
fn simulate_compiler_check(_code: &str) -> bool {
    true
}

/// Simulates a rule/convention check on the unit.
/// For now always succeeds; real linting rules will replace this.
fn simulate_rule_check(_unit_type: &str, _context_slug: &str) -> bool {
    true
}

async fn load_tool_message_row(pool: &DbPool, tool_message_id: Uuid) -> Result<ChatMessageRow, BoxError> {
    block_on_db_tokio(pool, move |conn| {
        chat_messages::table
            .filter(chat_messages::id.eq(tool_message_id))
            .select(ChatMessageRow::as_select())
            .first::<ChatMessageRow>(conn)
    })
    .await
    .map_err(|e| -> BoxError { e.to_string().into() })
}

fn extract_create_unit_payload(row: &ChatMessageRow) -> Result<ToolCreateUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("create_unit tool message has no payload")?;
    serde_json::from_value::<ToolCreateUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolCreateUnitPayload: {e}").into() })
}

fn extract_modify_unit_payload(row: &ChatMessageRow) -> Result<ToolModifyUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("modify_unit tool message has no payload")?;
    serde_json::from_value::<ToolModifyUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolModifyUnitPayload: {e}").into() })
}

fn extract_remove_unit_payload(row: &ChatMessageRow) -> Result<ToolRemoveUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("remove_unit tool message has no payload")?;
    serde_json::from_value::<ToolRemoveUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolRemoveUnitPayload: {e}").into() })
}

async fn update_tool_message_payload<P: serde::Serialize>(
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

async fn broadcast_message_updated(
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

/// Calls `maybe_complete_invoking_message` so the AI Text invoker is completed and the LLM
/// receives a queue continuation. Should be called whenever a tool message is set to Completed
/// (success or failure) in the reviewer — in both cases the LLM must be allowed to continue.
async fn trigger_llm_continuation(
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
