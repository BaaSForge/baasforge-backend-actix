use crate::services::message_queue::{
    DeadLetterSpec, PublishError, RetryPolicy, SharedRabbitPublisher, publish_with_channel,
};
use chrono::{DateTime, Utc};
use lapin::{Connection, Consumer};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub const PLATFORM_COST_CALCULATE_EXCHANGE: &str = "platform_cost_calculate_exchange";
pub const PLATFORM_COST_CALCULATE_WORK_QUEUE: &str = "platform_cost_calculate_work_queue";
pub const PLATFORM_COST_CALCULATE_WORK_QUEUE_BINDING: &str = "platform_cost_calculate.#";
pub const PLATFORM_COST_CALCULATE_ROUTING_KEY: &str = "platform_cost_calculate.calculate";

pub const PLATFORM_COST_CALCULATE_RETRY_DELAY_QUEUE: &str =
    "retry_delay_platform_cost_calculate_work_queue";

pub const PLATFORM_COST_CALCULATE_DEAD_LETTER_EXCHANGE: &str =
    "platform_cost_calculate_dead_letter_exchange";
pub const PLATFORM_COST_CALCULATE_DEAD_LETTER_QUEUE: &str =
    "platform_cost_calculate_dead_letter_queue";
pub const PLATFORM_COST_CALCULATE_DEAD_LETTER_BINDING: &str = "platform_cost_calculate.#";

pub const PLATFORM_COST_CALCULATE_MAX_RETRIES: u32 = 2;

pub fn cost_calculate_retry_policy(retry_delay_ttl_ms: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries: PLATFORM_COST_CALCULATE_MAX_RETRIES,
        retry_delay_ttl_ms,
    }
}

pub fn cost_calculate_dead_letter_spec() -> DeadLetterSpec {
    DeadLetterSpec {
        exchange_name: PLATFORM_COST_CALCULATE_DEAD_LETTER_EXCHANGE.to_string(),
        queue_name: PLATFORM_COST_CALCULATE_DEAD_LETTER_QUEUE.to_string(),
        binding_key: PLATFORM_COST_CALCULATE_DEAD_LETTER_BINDING.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCostCalculateMessage {
    pub project_id: Uuid,
    pub owner_user_id: Uuid,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub duration_seconds: i64,
    #[serde(default)]
    pub retry_num: u32,
}

pub async fn publish_cost_calculate_message(
    publisher: &Arc<SharedRabbitPublisher>,
    message: &PlatformCostCalculateMessage,
) -> Result<(), PublishError> {
    let payload = serde_json::to_vec(message).map_err(PublishError::Serialize)?;
    publish_with_channel(
        publisher.as_ref(),
        PLATFORM_COST_CALCULATE_EXCHANGE,
        PLATFORM_COST_CALCULATE_ROUTING_KEY,
        &payload,
    )
    .await
    .map_err(PublishError::Queue)
}

pub async fn create_cost_calculate_work_consumer_on_connection(
    connection: &Connection,
) -> Result<(lapin::Channel, Consumer), lapin::Error> {
    crate::services::message_queue::create_work_consumer_on_connection(
        connection,
        PLATFORM_COST_CALCULATE_EXCHANGE,
        PLATFORM_COST_CALCULATE_WORK_QUEUE,
        PLATFORM_COST_CALCULATE_WORK_QUEUE_BINDING,
    )
    .await
}
