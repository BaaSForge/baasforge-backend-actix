use crate::configs::Settings;
use crate::database::{establish_pool, DbPool};
use crate::services::message_queue::{
    create_shared_publisher, publish_to_dead_letter, publish_to_retry_delay, SharedRabbitPublisher,
};
use crate::services::unit_technical_reviewer::handlers::{
    process_create_unit_compiler_check, process_create_unit_rule_check,
    process_modify_unit_compiler_check, process_modify_unit_rule_check,
    process_remove_unit_rule_check,
};
use crate::services::unit_technical_reviewer::helpers::{fail_tool_message_and_continue, BoxError, RedisConn};
use crate::services::unit_technical_reviewer::queue::{
    create_reviewer_work_consumer_on_connection, reviewer_dead_letter_spec, reviewer_retry_policy,
    UnitReviewerMessageType, UnitReviewerQueueMessage, UNIT_REVIEWER_EXCHANGE,
    UNIT_REVIEWER_ROUTING_KEY,
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
    let retry_delay_exchange = settings.retry_delay_exchange.clone();
    let simulate_failure = settings.unit_reviewer_simulate_failure;
    info!("Connecting to RabbitMQ...");
    let connection = lapin::Connection::connect(&rabbit_url, ConnectionProperties::default())
        .await
        .map_err(|e| -> BoxError { e.into() })?;
    let (_consumer_channel, mut consumer) =
        create_reviewer_work_consumer_on_connection(&connection)
            .await
            .map_err(|e| -> BoxError { e.into() })?;
    // Separate channel for retry/DLQ publishes so failures don't block the consumer channel.
    let retry_channel = connection
        .create_channel()
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
            retry_num = message.retry_num,
            "Unit reviewer: processing message"
        );

        let retry_num = message.retry_num;
        let retry_policy = reviewer_retry_policy();
        let dead_letter_spec = reviewer_dead_letter_spec();

        let result = process_message(&pool, &redis_conn, &publisher, message.clone(), simulate_failure).await;
        if let Err(ref e) = result {
            if retry_policy.should_dead_letter(retry_num) {
                tracing::error!(
                    tool_message_id = %message.tool_message_id,
                    retry_num,
                    "Unit reviewer processing failed after max retries, sending to dead letter: {}",
                    e
                );
                // Mark the tool message as Completed/Failed and unblock the LLM before the
                // message lands in the DLQ. Without this the tool message stays Generating
                // forever and the LLM never receives a continuation.
                fail_tool_message_and_continue(
                    &pool,
                    &redis_conn,
                    &publisher,
                    &message,
                    &format!(
                        "Reviewer failed after {} attempt(s) and will not retry: {}",
                        retry_num + 1,
                        e
                    ),
                )
                .await;
                if let Err(dl_e) = publish_to_dead_letter(
                    &retry_channel,
                    &dead_letter_spec.exchange_name,
                    UNIT_REVIEWER_ROUTING_KEY,
                    &delivery.data,
                )
                .await
                {
                    tracing::error!("Failed to publish reviewer message to dead letter: {}", dl_e);
                }
            } else {
                let next_retry_num = retry_num + 1;
                tracing::error!(
                    tool_message_id = %message.tool_message_id,
                    retry_num,
                    next_retry_num,
                    "Unit reviewer processing failed, scheduling retry: {}",
                    e
                );
                let mut message_for_retry = message.clone();
                message_for_retry.retry_num = next_retry_num;
                match serde_json::to_vec(&message_for_retry) {
                    Ok(payload_bytes) => {
                        if let Err(retry_e) = publish_to_retry_delay(
                            &retry_channel,
                            &retry_delay_exchange,
                            UNIT_REVIEWER_ROUTING_KEY,
                            &payload_bytes,
                            retry_num,
                            &retry_policy,
                        )
                        .await
                        {
                            tracing::error!(
                                tool_message_id = %message.tool_message_id,
                                "Failed to publish reviewer message to retry delay: {}",
                                retry_e
                            );
                            // Fallback: send to DLQ so the message is not silently lost.
                            if let Err(dl_e) = publish_to_dead_letter(
                                &retry_channel,
                                &dead_letter_spec.exchange_name,
                                UNIT_REVIEWER_ROUTING_KEY,
                                &delivery.data,
                            )
                            .await
                            {
                                tracing::error!(
                                    "Failed to publish reviewer message to dead letter (after retry publish failure): {}",
                                    dl_e
                                );
                            }
                        }
                    }
                    Err(ser_e) => {
                        tracing::error!(
                            tool_message_id = %message.tool_message_id,
                            "Failed to serialize reviewer message for retry, sending to dead letter: {}",
                            ser_e
                        );
                        if let Err(dl_e) = publish_to_dead_letter(
                            &retry_channel,
                            &dead_letter_spec.exchange_name,
                            UNIT_REVIEWER_ROUTING_KEY,
                            &delivery.data,
                        )
                        .await
                        {
                            tracing::error!(
                                "Failed to publish reviewer message to dead letter (after serialize failure): {}",
                                dl_e
                            );
                        }
                    }
                }
            }
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
    simulate_failure: bool,
) -> Result<(), BoxError> {
    // Set UNIT_REVIEWER_SIMULATE_FAILURE=true in .env.local to force every attempt to fail,
    // exercising the full retry → dead-letter → fail_tool_message_and_continue path.
    if simulate_failure {
        return Err("simulated reviewer failure (UNIT_REVIEWER_SIMULATE_FAILURE=true)".into());
    }

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
