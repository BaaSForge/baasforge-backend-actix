use std::time::Duration;

use crate::configs::Settings;
use crate::database::{block_on_db_tokio, establish_pool, DbPool};
use crate::services::message_queue::{
    publish_to_dead_letter, publish_to_retry_delay, wait_for_consumer_recovery,
    wait_for_rabbit_connection,
};
use crate::services::balance::{get_user_balance, set_billing_frozen};
use crate::services::pricing::platform_rates_for_package_tier;
use crate::services::platform_cost_calculate::db;
use crate::services::platform_cost_calculate::meter;
use crate::services::platform_cost_calculate::queue::{
    self, PlatformCostCalculateMessage, PLATFORM_COST_CALCULATE_ROUTING_KEY,
    cost_calculate_dead_letter_spec, cost_calculate_retry_policy,
    create_cost_calculate_work_consumer_on_connection,
};
use lapin::options::{BasicAckOptions, BasicNackOptions};
use tokio_stream::StreamExt;
use tracing::{info, warn};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub async fn run() -> Result<(), BoxError> {
    tracing_subscriber::fmt::init();

    let settings = Settings::new().map_err(|e| -> BoxError { e.to_string().into() })?;
    crate::configs::start_usage_publisher_if_enabled(&settings);

    let pool = establish_pool(
        settings.pg_database_url.clone(),
        settings.database_pool_size,
        settings.database_connection_timeout_secs,
        settings.database_pool_idle_timeout_secs,
    );

    let rabbit_url = settings.rabbit_url.clone();
    let retry_delay_exchange = settings.retry_delay_exchange.clone();
    let rabbit_recovery_wait =
        Duration::from_secs(settings.rabbit_worker_recovery_wait_secs);
    let rabbit_connect_retry =
        Duration::from_secs(settings.rabbit_connect_retry_interval_secs);

    info!("Connecting to RabbitMQ...");
    let connection = wait_for_rabbit_connection(&rabbit_url, rabbit_connect_retry).await;
    let (consumer_channel, mut consumer) =
        create_cost_calculate_work_consumer_on_connection(&connection)
            .await
            .map_err(|e| -> BoxError { e.into() })?;
    let retry_channel = connection
        .create_channel()
        .await
        .map_err(|e| -> BoxError { e.into() })?;

    info!("Platform cost calculate consumer started");

    let retry_policy = cost_calculate_retry_policy(settings.retry_delay_ttl_ms);
    let dead_letter_spec = cost_calculate_dead_letter_spec();

    while let Some(delivery_result) = consumer.next().await {
        let delivery = match delivery_result {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "consumer error, waiting for recovery");
                wait_for_consumer_recovery(
                    &consumer_channel,
                    e,
                    rabbit_recovery_wait,
                    rabbit_connect_retry,
                )
                .await;
                continue;
            }
        };

        let message = match serde_json::from_slice::<PlatformCostCalculateMessage>(&delivery.data) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "invalid platform cost calculate message");
                if delivery
                    .acker
                    .ack(BasicAckOptions::default())
                    .await
                    .is_err()
                {
                    warn!("failed to ack invalid platform cost calculate delivery");
                }
                continue;
            }
        };

        let retry_num = message.retry_num;
        let delivery_data = delivery.data.as_slice();

        let mut should_ack = true;

        if let Err(e) = process_message(&pool, message.clone()).await {
            warn!(
                error = %e,
                project_id = %message.project_id,
                retry_num,
                "platform cost calculate failed"
            );

            if retry_policy.should_dead_letter(retry_num) {
                if let Err(dlq_err) = publish_to_dead_letter(
                    &retry_channel,
                    &dead_letter_spec.exchange_name,
                    PLATFORM_COST_CALCULATE_ROUTING_KEY,
                    delivery_data,
                    rabbit_recovery_wait,
                )
                .await
                {
                    warn!(error = %dlq_err, "failed to publish to dead letter queue");
                    should_ack = false;
                }
            } else {
                let next_retry_num = retry_num + 1;
                let mut retry_message = message;
                retry_message.retry_num = next_retry_num;
                match serde_json::to_vec(&retry_message) {
                    Ok(payload) => {
                        if let Err(retry_err) = publish_to_retry_delay(
                            &retry_channel,
                            &retry_delay_exchange,
                            PLATFORM_COST_CALCULATE_ROUTING_KEY,
                            &payload,
                            retry_num,
                            &retry_policy,
                            rabbit_recovery_wait,
                        )
                        .await
                        {
                            warn!(error = %retry_err, "failed to publish to retry delay queue");
                            if let Err(dlq_err) = publish_to_dead_letter(
                                &retry_channel,
                                &dead_letter_spec.exchange_name,
                                PLATFORM_COST_CALCULATE_ROUTING_KEY,
                                delivery_data,
                                rabbit_recovery_wait,
                            )
                            .await
                            {
                                warn!(
                                    error = %dlq_err,
                                    "failed to publish to dead letter queue after retry failure"
                                );
                                should_ack = false;
                            }
                        }
                    }
                    Err(ser_e) => {
                        warn!(error = %ser_e, "failed to serialize message for retry");
                        if let Err(dlq_err) = publish_to_dead_letter(
                            &retry_channel,
                            &dead_letter_spec.exchange_name,
                            PLATFORM_COST_CALCULATE_ROUTING_KEY,
                            delivery_data,
                            rabbit_recovery_wait,
                        )
                        .await
                        {
                            warn!(
                                error = %dlq_err,
                                "failed to publish to dead letter queue after serialize failure"
                            );
                            should_ack = false;
                        }
                    }
                }
            }
        }

        if should_ack {
            if delivery
                .acker
                .ack(BasicAckOptions::default())
                .await
                .is_err()
            {
                warn!("failed to ack platform cost calculate delivery");
            }
        } else if delivery
            .acker
            .nack(BasicNackOptions {
                requeue: true,
                multiple: false,
            })
            .await
            .is_err()
        {
            warn!("failed to nack platform cost calculate delivery for redelivery");
        }
    }

    let flush_wait = settings.usage_publish_flush_ms.saturating_add(500);
    tokio::time::sleep(Duration::from_millis(flush_wait)).await;
    Ok(())
}

async fn process_message(pool: &DbPool, message: PlatformCostCalculateMessage) -> Result<(), String> {
    if message.duration_seconds <= 0 {
        return Ok(());
    }

    let project_id = message.project_id;
    let owner_user_id = message.owner_user_id;
    let duration_seconds = message.duration_seconds;
    let recorded_at = message.period_end;
    let period_start = message.period_start;
    let period_end = message.period_end;

    let (unit_count, stage_runtime_count, package_tier, has_subscription, is_annual) =
        block_on_db_tokio(pool, move |conn| {
            let unit_count = db::count_live_units(conn, project_id)?;
            let stage_runtime_count = db::count_stage_runtimes(conn, project_id)?;
            let package_tier = db::user_package_tier(conn, owner_user_id)?;
            let (has_subscription, is_annual) =
                db::user_has_paid_subscription(conn, owner_user_id)?;
            Ok::<_, diesel::result::Error>((
                unit_count,
                stage_runtime_count,
                package_tier,
                has_subscription,
                is_annual,
            ))
        })
        .await
        .map_err(|e| e.to_string())?;

    meter::enqueue_project_meter(project_id, duration_seconds, recorded_at);
    meter::enqueue_unit_meter(project_id, unit_count, duration_seconds, recorded_at);
    meter::enqueue_stage_runtime_meter(
        project_id,
        stage_runtime_count,
        duration_seconds,
        recorded_at,
    );

    if has_subscription {
        let rates = platform_rates_for_package_tier(package_tier);
        if rates.subscription_per_hour > 0.0 {
            let claimed = block_on_db_tokio(pool, move |conn| {
                db::try_claim_subscription_cost_window(conn, owner_user_id, period_start, period_end)
            })
            .await
            .map_err(|e| e.to_string())?;

            if claimed {
                meter::enqueue_subscription_meter(
                    project_id,
                    duration_seconds,
                    recorded_at,
                    owner_user_id,
                    package_tier,
                    is_annual,
                );

                let should_freeze = block_on_db_tokio(pool, move |conn| {
                    let balance = get_user_balance(conn, owner_user_id)?;
                    if balance.balance <= 0.0 {
                        set_billing_frozen(conn, owner_user_id, true)?;
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                })
                .await
                .map_err(|e| e.to_string())?;

                if should_freeze {
                    info!(
                        user_id = %owner_user_id,
                        "user billing frozen due to non-positive balance"
                    );
                }
            }
        }
    }

    info!(
        project_id = %project_id,
        duration_seconds,
        unit_count,
        stage_runtime_count,
        package_tier,
        "published platform cost usage to NATS"
    );
    Ok(())
}
