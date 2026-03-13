use lapin::{
    BasicProperties, Connection, ConnectionProperties, Consumer, ExchangeKind,
    options::*, types::{AMQPValue, FieldTable},
};
use std::borrow::Borrow;
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

async fn ensure_exchange(
    rabbit_url: &str,
    exchange_name: &str,
) -> Result<(Connection, lapin::Channel), lapin::Error> {
    let connection = Connection::connect(rabbit_url, ConnectionProperties::default()).await?;
    let channel = connection.create_channel().await?;

    let mut opts = ExchangeDeclareOptions::default();
    opts.durable = true;

    channel
        .exchange_declare(
            exchange_name,
            ExchangeKind::Topic,
            opts,
            FieldTable::default(),
        )
        .await?;

    Ok((connection, channel))
}

/// Spec for a delay queue: messages published to the retry delay exchange with a key matching
/// `retry_binding_key` land in this queue, wait `ttl_ms`, then are dead-lettered to
/// `dead_letter_exchange`. If `dead_letter_routing_key` is None, the message's original routing
/// key is preserved when dead-lettering (required for topic consumers that parse the key).
#[derive(Clone, Debug)]
pub struct RetryDelaySpec {
    /// Queue name for the delay queue (e.g. "delay_3s_chat_session_work_queue").
    pub delay_queue_name: String,
    /// Binding key (e.g. "chat_session.#") so the delay queue receives retries from the retry exchange.
    pub retry_binding_key: String,
    /// Exchange to send messages to when TTL expires (typically the main work exchange).
    pub dead_letter_exchange: String,
    /// If Some, used as routing key when dead-lettering. If None, original routing key is preserved.
    pub dead_letter_routing_key: Option<String>,
}

/// Declares a durable topic exchange on an existing channel. Idempotent.
/// Call this before declaring delay queues whose dead-letter-exchange points to this exchange,
/// so the DLX exists when the delay queue is declared (avoids broker closing the connection).
pub async fn declare_exchange_on_channel(
    channel: &lapin::Channel,
    exchange_name: &str,
) -> Result<(), lapin::Error> {
    let mut opts = ExchangeDeclareOptions::default();
    opts.durable = true;
    channel
        .exchange_declare(
            exchange_name,
            ExchangeKind::Topic,
            opts,
            FieldTable::default(),
        )
        .await?;
    Ok(())
}

/// Ensures the retry delay exchange and all given delay queues exist using an existing channel.
/// Idempotent. Use this in the worker with the same connection that will be used for consuming,
/// so no short-lived connection is dropped (which can cause "connection aborted" in lapin).
pub async fn ensure_retry_delay_infrastructure_with_channel(
    channel: &lapin::Channel,
    retry_delay_exchange: &str,
    ttl_ms: u32,
    specs: &[RetryDelaySpec],
) -> Result<(), lapin::Error> {
    let mut exchange_opts = ExchangeDeclareOptions::default();
    exchange_opts.durable = true;
    channel
        .exchange_declare(
            retry_delay_exchange,
            ExchangeKind::Topic,
            exchange_opts,
            FieldTable::default(),
        )
        .await?;

    for spec in specs {
        let mut args = FieldTable::default();
        args.insert(
            "x-message-ttl".into(),
            AMQPValue::LongInt(ttl_ms as i32),
        );
        args.insert(
            "x-dead-letter-exchange".into(),
            AMQPValue::ShortString(spec.dead_letter_exchange.as_str().into()),
        );
        if let Some(ref key) = spec.dead_letter_routing_key {
            args.insert(
                "x-dead-letter-routing-key".into(),
                AMQPValue::ShortString(key.as_str().into()),
            );
        }

        let mut queue_opts = QueueDeclareOptions::default();
        queue_opts.durable = true;
        let queue = channel
            .queue_declare(
                spec.delay_queue_name.as_str(),
                queue_opts,
                args,
            )
            .await?;
        let queue_name_borrow = queue.borrow();
        channel
            .queue_bind(
                queue_name_borrow,
                retry_delay_exchange,
                spec.retry_binding_key.as_str(),
                QueueBindOptions::default(),
                FieldTable::default(),
            )
            .await?;
    }

    Ok(())
}

/// Ensures the retry delay exchange and all given delay queues exist. Idempotent; safe to call at
/// startup from the API. Creates a short-lived connection and closes it gracefully.
pub async fn ensure_retry_delay_infrastructure(
    rabbit_url: &str,
    retry_delay_exchange: &str,
    ttl_ms: u32,
    specs: &[RetryDelaySpec],
) -> Result<(), lapin::Error> {
    let connection = Connection::connect(rabbit_url, ConnectionProperties::default()).await?;
    let channel = connection.create_channel().await?;
    ensure_retry_delay_infrastructure_with_channel(&channel, retry_delay_exchange, ttl_ms, specs)
        .await?;
    // Close gracefully so lapin's IO loop doesn't report "connection aborted" when we drop.
    use lapin::types::ReplyCode;
    let _ = connection
        .close(ReplyCode::try_from(200u16).unwrap_or_default(), "OK")
        .await;
    Ok(())
}

/// Publishes a failed message to the retry delay exchange so it will be held for the queue's TTL
/// then dead-lettered back to the work exchange. Use the original delivery `routing_key` (e.g.
/// "chat_session.1.42") so that when the message is dead-lettered without a fixed DL routing key,
/// the worker receives a parseable routing key.
pub async fn publish_to_retry_delay(
    channel: &lapin::Channel,
    retry_delay_exchange: &str,
    routing_key: &str,
    payload: &[u8],
) -> Result<(), lapin::Error> {
    let props = BasicProperties::default().with_delivery_mode(2);
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

pub struct SharedRabbitPublisher {
    _connection: Connection,
    channel: lapin::Channel,
}

pub async fn create_shared_publisher(
    rabbit_url: &str,
    exchange_name: &str,
) -> Result<Arc<SharedRabbitPublisher>, lapin::Error> {
    let (connection, channel) = ensure_exchange(rabbit_url, exchange_name).await?;
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

/// Creates a consumer for a durable shared queue on an existing connection.
/// Use in the worker so the same connection can be used for retry delay setup and consuming,
/// avoiding short-lived connections that can trigger "connection aborted" when dropped.
pub async fn create_work_consumer_on_connection(
    connection: &Connection,
    exchange_name: &str,
    queue_name: &str,
    binding_pattern: &str,
) -> Result<(lapin::Channel, Consumer), lapin::Error> {
    let channel = connection.create_channel().await?;
    let mut opts = ExchangeDeclareOptions::default();
    opts.durable = true;
    channel
        .exchange_declare(
            exchange_name,
            ExchangeKind::Topic,
            opts,
            FieldTable::default(),
        )
        .await?;

    let mut queue_opts = QueueDeclareOptions::default();
    queue_opts.durable = true;
    let queue = channel
        .queue_declare(queue_name, queue_opts, FieldTable::default())
        .await?;
    let queue_name_borrow = queue.borrow();
    channel
        .queue_bind(
            queue_name_borrow,
            exchange_name,
            binding_pattern,
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;
    channel
        .basic_qos(1, BasicQosOptions::default())
        .await?;
    let consumer_tag = format!("work_consumer_{}", queue_name);
    let mut consume_opts = BasicConsumeOptions::default();
    consume_opts.no_ack = false;
    let consumer = channel
        .basic_consume(
            queue_name_borrow,
            &consumer_tag,
            consume_opts,
            FieldTable::default(),
        )
        .await?;
    Ok((channel, consumer))
}
