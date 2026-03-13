use lapin::{
    BasicProperties, Connection, ConnectionProperties, Consumer,
    options::*, types::{FieldTable, ShortString},
};
use std::fmt;
use std::sync::Arc;

#[derive(Debug)]
pub enum PublishError {
    Serialize(serde_json::Error),
    Queue(lapin::Error),
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PublishError::Serialize(e) => write!(f, "serialize chat message: {}", e),
            PublishError::Queue(e) => write!(f, "message queue: {}", e),
        }
    }
}

impl std::error::Error for PublishError {}

/// Policy for retries: max attempts and exponential delay. Reusable for any channel.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Maximum number of retries (0 = no retries, 1 = one retry, etc.). After this, message goes to dead letter.
    pub max_retries: u32,
}

impl RetryPolicy {
    /// Delay in milliseconds before the next attempt. Formula: 1000 * 3^(1 + retry_num).
    /// retry_num 0 => 3s, 1 => 9s, 2 => 27s.
    pub fn retry_delay_ms(&self, retry_num: u32) -> u64 {
        const BASE_SECS: u64 = 3;
        1000 * BASE_SECS.pow(1 + retry_num)
    }

    /// True if this retry_num has exceeded max_retries (should send to dead letter).
    pub fn should_dead_letter(&self, retry_num: u32) -> bool {
        retry_num >= self.max_retries
    }
}

/// Spec for a delay queue: messages published to the retry delay exchange with a key matching
/// `retry_binding_key` land in this queue, wait (per-message TTL or queue TTL), then are
/// dead-lettered to `dead_letter_exchange`. If `dead_letter_routing_key` is None, the message's
/// original routing key is preserved when dead-lettering (required for topic consumers that parse the key).
#[derive(Clone, Debug)]
pub struct RetryDelaySpec {
    /// Queue name for the retry delay queue (e.g. "retry_delay_chat_session_work_queue").
    pub delay_queue_name: String,
    /// Binding key (e.g. "chat_session.#") so the delay queue receives retries from the retry exchange.
    pub retry_binding_key: String,
    /// Exchange to send messages to when TTL expires (typically the main work exchange).
    pub dead_letter_exchange: String,
    /// If Some, used as routing key when dead-lettering. If None, original routing key is preserved.
    pub dead_letter_routing_key: Option<String>,
    /// Optional queue-level TTL in ms. If None, use per-message expiration only when publishing.
    pub queue_ttl_ms: Option<u32>,
}

/// Spec for a dead letter destination: exchange and optional routing key for messages that exceeded max retries.
#[derive(Clone, Debug)]
pub struct DeadLetterSpec {
    pub exchange_name: String,
    pub queue_name: String,
    pub binding_key: String,
}

/// Publishes a failed message to the retry delay exchange with per-message TTL (exponential backoff).
/// Delay is `policy.retry_delay_ms(retry_num)` ms. Use the original delivery `routing_key` so that
/// when the message is dead-lettered back to the work exchange, the worker receives a parseable key.
pub async fn publish_to_retry_delay(
    channel: &lapin::Channel,
    retry_delay_exchange: &str,
    routing_key: &str,
    payload: &[u8],
    retry_num: u32,
    policy: &RetryPolicy,
) -> Result<(), lapin::Error> {
    let delay_ms = policy.retry_delay_ms(retry_num);
    let expiration = ShortString::from(delay_ms.to_string());
    let props = BasicProperties::default()
        .with_delivery_mode(2)
        .with_expiration(expiration);
    channel
        .basic_publish(
            retry_delay_exchange,
            routing_key,
            BasicPublishOptions::default(),
            payload,
            props,
        )
        .await?
        .await?;
    Ok(())
}

/// Publishes a message that exceeded max retries to the dead letter exchange. No TTL; messages stay for later processing/logging.
pub async fn publish_to_dead_letter(
    channel: &lapin::Channel,
    dead_letter_exchange: &str,
    routing_key: &str,
    payload: &[u8],
) -> Result<(), lapin::Error> {
    let props = BasicProperties::default().with_delivery_mode(2);
    channel
        .basic_publish(
            dead_letter_exchange,
            routing_key,
            BasicPublishOptions::default(),
            payload,
            props,
        )
        .await?
        .await?;
    Ok(())
}

pub struct SharedRabbitPublisher {
    _connection: Connection,
    channel: lapin::Channel,
}

/// Creates a shared publisher connection and channel. Assumes exchanges/queues are already created (e.g. via queue_setup_http.sh).
pub async fn create_shared_publisher(
    rabbit_url: &str,
    _exchange_name: &str,
) -> Result<Arc<SharedRabbitPublisher>, lapin::Error> {
    let connection = Connection::connect(rabbit_url, ConnectionProperties::default()).await?;
    let channel = connection.create_channel().await?;
    Ok(Arc::new(SharedRabbitPublisher {
        _connection: connection,
        channel,
    }))
}

pub async fn publish_with_channel(
    publisher: &SharedRabbitPublisher,
    exchange_name: &str,
    routing_key: &str,
    payload: &[u8],
) -> Result<(), lapin::Error> {
    let props = BasicProperties::default().with_delivery_mode(2);
    publisher
        .channel
        .basic_publish(
            exchange_name,
            routing_key,
            BasicPublishOptions::default(),
            payload,
            props,
        )
        .await?
        .await?;
    Ok(())
}

/// Creates a consumer for a durable queue on an existing connection.
/// Assumes the queue and its binding already exist (e.g. created by queue_setup_http.sh).
pub async fn create_work_consumer_on_connection(
    connection: &Connection,
    _exchange_name: &str,
    queue_name: &str,
    _binding_pattern: &str,
) -> Result<(lapin::Channel, Consumer), lapin::Error> {
    let channel = connection.create_channel().await?;
    channel
        .basic_qos(1, BasicQosOptions::default())
        .await?;
    let consumer_tag = format!("work_consumer_{}", queue_name);
    let mut consume_opts = BasicConsumeOptions::default();
    consume_opts.no_ack = false;
    let consumer = channel
        .basic_consume(
            queue_name,
            &consumer_tag,
            consume_opts,
            FieldTable::default(),
        )
        .await?;
    Ok((channel, consumer))
}
