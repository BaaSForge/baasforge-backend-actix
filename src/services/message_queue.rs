use lapin::{
    BasicProperties, Connection, ConnectionProperties, Consumer, ExchangeKind,
    options::*, types::FieldTable,
};
use std::borrow::Borrow;
use std::fmt;

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

pub async fn ensure_exchange(
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

/// Publishes a message to the exchange. Uses persistent delivery so messages
/// survive broker restarts (important for work queues and at scale).
pub async fn publish(
    rabbit_url: &str,
    exchange_name: &str,
    routing_key: &str,
    payload: &[u8],
) -> Result<(), lapin::Error> {
    let (_connection, channel) = ensure_exchange(rabbit_url, exchange_name).await?;

    let props = BasicProperties::default().with_delivery_mode(2); // persistent

    channel
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

/// Creates a consumer for a durable shared queue (competing consumers).
/// Use for worker processes: durable queue, manual ack, prefetch 1 to avoid
/// one worker hoarding messages and to survive restarts.
pub async fn create_work_consumer(
    rabbit_url: &str,
    exchange_name: &str,
    queue_name: &str,
    binding_pattern: &str,
) -> Result<(Connection, Consumer), lapin::Error> {
    let (connection, channel) = ensure_exchange(rabbit_url, exchange_name).await?;

    let mut queue_opts = QueueDeclareOptions::default();
    queue_opts.durable = true; // survive broker restart; required for competing consumers

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
        .basic_qos(1, BasicQosOptions::default()) // prefetch 1: fair dispatch, no hoarding
        .await?;

    let consumer_tag = format!("work_consumer_{}", queue_name);
    let mut consume_opts = BasicConsumeOptions::default();
    consume_opts.no_ack = false; // manual ack so messages are redelivered on crash

    let consumer = channel
        .basic_consume(
            queue_name_borrow,
            &consumer_tag,
            consume_opts,
            FieldTable::default(),
        )
        .await?;

    Ok((connection, consumer))
}
