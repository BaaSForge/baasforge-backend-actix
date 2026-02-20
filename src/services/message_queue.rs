use lapin::{BasicProperties, Connection, ConnectionProperties, Consumer, ExchangeKind, options::*, types::FieldTable};
use std::borrow::Borrow;

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

pub async fn publish(
    rabbit_url: &str,
    exchange_name: &str,
    routing_key: &str,
    payload: &[u8],
) -> Result<(), lapin::Error> {
    let (_connection, channel) = ensure_exchange(rabbit_url, exchange_name).await?;

    channel
        .basic_publish(
            exchange_name,
            routing_key,
            BasicPublishOptions::default(),
            payload,
            BasicProperties::default(),
        )
        .await?
        .await?;

    Ok(())
}

pub async fn create_consumer(
    rabbit_url: &str,
    exchange_name: &str,
    routing_key: &str,
) -> Result<(Connection, Consumer), lapin::Error> {
    let (connection, channel) = ensure_exchange(rabbit_url, exchange_name).await?;

    let mut queue_opts = QueueDeclareOptions::default();
    queue_opts.exclusive = true;
    queue_opts.auto_delete = true;

    let queue = channel
        .queue_declare("", queue_opts, FieldTable::default())
        .await?;

    let queue_name = queue.borrow();
    channel
        .queue_bind(
            queue_name,
            exchange_name,
            routing_key,
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let consumer_tag = format!("consumer_{}_{}", exchange_name, routing_key);
    let mut consume_opts = BasicConsumeOptions::default();
    consume_opts.no_ack = true;

    let consumer = channel
        .basic_consume(
            queue_name,
            &consumer_tag,
            consume_opts,
            FieldTable::default(),
        )
        .await?;

    Ok((connection, consumer))
}
