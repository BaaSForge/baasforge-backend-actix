use lapin::{
    BasicProperties, Channel, Connection, ConnectionProperties, Consumer,
    options::*,
    types::{FieldTable, ShortString},
};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// Lapin auto-recovery: reconnects and replays topology after broker/network failures.
pub fn rabbit_connection_properties() -> ConnectionProperties {
    ConnectionProperties::default()
        .enable_auto_recover()
        .with_connection_name("baal-backend".into())
        .configure_backoff(|b| {
            b.with_min_delay(Duration::from_secs(1))
                .with_max_delay(Duration::from_secs(30))
                .with_max_times(usize::MAX)
        })
}

pub async fn connect_rabbit(rabbit_url: &str) -> Result<Connection, lapin::Error> {
    Connection::connect(rabbit_url, rabbit_connection_properties()).await
}

pub async fn connect_rabbit_timed(
    rabbit_url: &str,
    timeout: Duration,
) -> Result<Connection, lapin::Error> {
    match tokio::time::timeout(timeout, connect_rabbit(rabbit_url)).await {
        Ok(result) => result,
        Err(_) => Err(rabbit_timed_out_lapin_error()),
    }
}

fn rabbit_timed_out_lapin_error() -> lapin::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "rabbit operation timed out").into()
}

async fn basic_publish_once(
    channel: &Channel,
    exchange: &str,
    routing_key: &str,
    payload: &[u8],
    props: BasicProperties,
) -> Result<(), lapin::Error> {
    channel
        .basic_publish(
            exchange.into(),
            routing_key.into(),
            BasicPublishOptions::default(),
            payload,
            props,
        )
        .await?
        .await?;
    Ok(())
}

async fn wait_for_recovery_timed(
    channel: &Channel,
    error: lapin::Error,
    recovery_wait: Duration,
) -> Result<(), lapin::Error> {
    match tokio::time::timeout(recovery_wait, channel.wait_for_recovery(error)).await {
        Ok(result) => result,
        Err(_) => Err(rabbit_timed_out_lapin_error()),
    }
}

/// One publish attempt; on failure wait (bounded) for lapin recovery, then retry once.
async fn basic_publish_with_recovery(
    channel: &Channel,
    exchange: &str,
    routing_key: &str,
    payload: &[u8],
    props: BasicProperties,
    recovery_wait: Duration,
) -> Result<(), lapin::Error> {
    let err = match basic_publish_once(channel, exchange, routing_key, payload, props.clone()).await
    {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    wait_for_recovery_timed(channel, err, recovery_wait).await?;
    basic_publish_once(channel, exchange, routing_key, payload, props).await
}

pub async fn wait_for_rabbit_connection(
    rabbit_url: &str,
    retry_interval: Duration,
) -> Connection {
    loop {
        match connect_rabbit(rabbit_url).await {
            Ok(connection) => return connection,
            Err(e) => {
                tracing::warn!(
                    retry_interval_secs = retry_interval.as_secs(),
                    %e,
                    "Rabbit connect failed, retrying"
                );
                tokio::time::sleep(retry_interval).await;
            }
        }
    }
}

pub async fn wait_for_shared_publisher(
    rabbit_url: &str,
    exchange_name: &str,
    recovery_wait: Duration,
    retry_interval: Duration,
) -> Arc<SharedRabbitPublisher> {
    loop {
        match create_shared_publisher(rabbit_url, exchange_name, recovery_wait).await {
            Ok(publisher) => return publisher,
            Err(e) => {
                tracing::warn!(
                    retry_interval_secs = retry_interval.as_secs(),
                    %e,
                    "Rabbit publisher connect failed, retrying"
                );
                tokio::time::sleep(retry_interval).await;
            }
        }
    }
}

/// Bounded consumer recovery: never blocks unbounded; sleeps on timeout to avoid tight loops.
pub async fn wait_for_consumer_recovery(
    channel: &Channel,
    error: lapin::Error,
    recovery_wait: Duration,
    backoff: Duration,
) {
    match tokio::time::timeout(recovery_wait, channel.wait_for_recovery(error)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(%e, "Rabbit consumer error is not recoverable");
        }
        Err(_) => {
            tracing::warn!(
                recovery_wait_secs = recovery_wait.as_secs(),
                "Rabbit consumer recovery timed out, backing off"
            );
            tokio::time::sleep(backoff).await;
        }
    }
}

#[derive(Debug)]
pub enum PublishError {
    Serialize(serde_json::Error),
    Queue(lapin::Error),
    TimedOut,
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PublishError::Serialize(e) => write!(f, "serialize chat message: {}", e),
            PublishError::Queue(e) => write!(f, "message queue: {}", e),
            PublishError::TimedOut => write!(f, "rabbit operation timed out"),
        }
    }
}

impl std::error::Error for PublishError {}

/// Policy for retries: max attempts and exponential delay before re-delivery. Reusable for any channel.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Maximum number of retries (0 = no retries, 1 = one retry, etc.). After this, message goes to dead letter.
    pub max_retries: u32,
    /// Exponential backoff base (`RETRY_DELAY_TTL_MS` / 1000); replaces the old hardcoded `BASE_SECS = 4`.
    pub retry_delay_ttl_ms: u32,
}

impl RetryPolicy {
    /// Delay before the next attempt: `1000 * base^(1 + retry_num)` where `base = retry_delay_ttl_ms / 1000`.
    /// e.g. 3000 → 3s, 9s, 27s; 4000 → 4s, 16s, 64s (previous default).
    pub fn retry_delay_ms(&self, retry_num: u32) -> u64 {
        let base_secs = (self.retry_delay_ttl_ms as u64 / 1000).max(1);
        1000 * base_secs.pow(1 + retry_num)
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
    recovery_wait: Duration,
) -> Result<(), lapin::Error> {
    let delay_ms = policy.retry_delay_ms(retry_num);
    let expiration = ShortString::from(delay_ms.to_string());
    let props = BasicProperties::default()
        .with_delivery_mode(2)
        .with_expiration(expiration);
    basic_publish_with_recovery(
        channel,
        retry_delay_exchange,
        routing_key,
        payload,
        props,
        recovery_wait,
    )
    .await
}

/// Publishes a message that exceeded max retries to the dead letter exchange. No TTL; messages stay for later processing/logging.
pub async fn publish_to_dead_letter(
    channel: &lapin::Channel,
    dead_letter_exchange: &str,
    routing_key: &str,
    payload: &[u8],
    recovery_wait: Duration,
) -> Result<(), lapin::Error> {
    let props = BasicProperties::default().with_delivery_mode(2);
    basic_publish_with_recovery(
        channel,
        dead_letter_exchange,
        routing_key,
        payload,
        props,
        recovery_wait,
    )
    .await
}

pub struct SharedRabbitPublisher {
    _connection: Connection,
    channel: lapin::Channel,
    recovery_wait: Duration,
}

/// Creates a shared publisher connection and channel. Assumes exchanges/queues are already created (e.g. via queue_setup_http.sh).
pub async fn create_shared_publisher(
    rabbit_url: &str,
    _exchange_name: &str,
    recovery_wait: Duration,
) -> Result<Arc<SharedRabbitPublisher>, lapin::Error> {
    let connection = connect_rabbit(rabbit_url).await?;
    let channel = connection.create_channel().await?;
    Ok(Arc::new(SharedRabbitPublisher {
        _connection: connection,
        channel,
        recovery_wait,
    }))
}

pub async fn create_shared_publisher_timed(
    rabbit_url: &str,
    exchange_name: &str,
    recovery_wait: Duration,
    timeout: Duration,
) -> Result<Arc<SharedRabbitPublisher>, lapin::Error> {
    match tokio::time::timeout(
        timeout,
        create_shared_publisher(rabbit_url, exchange_name, recovery_wait),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(rabbit_timed_out_lapin_error()),
    }
}

pub async fn publish_with_channel(
    publisher: &SharedRabbitPublisher,
    exchange_name: &str,
    routing_key: &str,
    payload: &[u8],
) -> Result<(), lapin::Error> {
    let props = BasicProperties::default().with_delivery_mode(2);
    basic_publish_with_recovery(
        &publisher.channel,
        exchange_name,
        routing_key,
        payload,
        props,
        publisher.recovery_wait,
    )
    .await
}

pub async fn publish_with_channel_timed(
    publisher: &SharedRabbitPublisher,
    exchange_name: &str,
    routing_key: &str,
    payload: &[u8],
    timeout: Duration,
) -> Result<(), PublishError> {
    let props = BasicProperties::default().with_delivery_mode(2);
    match tokio::time::timeout(
        timeout,
        basic_publish_once(
            &publisher.channel,
            exchange_name,
            routing_key,
            payload,
            props,
        ),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(PublishError::Queue(e)),
        Err(_) => Err(PublishError::TimedOut),
    }
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
    channel.basic_qos(1, BasicQosOptions::default()).await?;
    let consumer_tag = format!("work_consumer_{}", queue_name);
    let mut consume_opts = BasicConsumeOptions::default();
    consume_opts.no_ack = false;
    let consumer = channel
        .basic_consume(
            queue_name.into(),
            consumer_tag.into(),
            consume_opts,
            FieldTable::default(),
        )
        .await?;
    Ok((channel, consumer))
}
