use crate::configs::Settings;
use crate::database::{establish_pool, DbPool};
use crate::services::message_queue::{create_shared_publisher, SharedRabbitPublisher};
use crate::services::unit_technical_reviewer::handlers::{
    process_create_unit_compiler_check, process_create_unit_rule_check,
    process_modify_unit_compiler_check, process_modify_unit_rule_check,
    process_remove_unit_rule_check,
};
use crate::services::unit_technical_reviewer::helpers::{BoxError, RedisConn};
use crate::services::unit_technical_reviewer::queue::{
    create_reviewer_work_consumer_on_connection, UnitReviewerMessageType, UnitReviewerQueueMessage,
    UNIT_REVIEWER_EXCHANGE,
};
use lapin::options::BasicAckOptions;
use lapin::ConnectionProperties;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::info;

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
