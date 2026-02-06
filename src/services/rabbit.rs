use lapin::{Connection, ConnectionProperties, ExchangeKind, options::*, types::FieldTable};
use std::borrow::Borrow;

pub const CHAT_SESSION_EXCHANGE: &str = "chat_session_exchange";

pub fn chat_session_routing_key(project_id: i64, session_id: i64) -> String {
    format!("chat_session.{}.{}", project_id, session_id)
}

pub async fn ensure_queue_channel(
    rabbit_url: &str,
    queue_name: &str,
) -> Result<(Connection, lapin::Channel), lapin::Error> {
    let connection = Connection::connect(rabbit_url, ConnectionProperties::default()).await?;
    let channel = connection.create_channel().await?;

    let mut opts = QueueDeclareOptions::default();
    opts.durable = true;

    channel
        .queue_declare(queue_name, opts, FieldTable::default())
        .await?;

    Ok((connection, channel))
}

pub async fn ensure_chat_session_exchange(
    rabbit_url: &str,
) -> Result<(Connection, lapin::Channel), lapin::Error> {
    let connection = Connection::connect(rabbit_url, ConnectionProperties::default()).await?;
    let channel = connection.create_channel().await?;

    let mut opts = ExchangeDeclareOptions::default();
    opts.durable = true;

    channel
        .exchange_declare(
            CHAT_SESSION_EXCHANGE,
            ExchangeKind::Topic,
            opts,
            FieldTable::default(),
        )
        .await?;

    Ok((connection, channel))
}

pub async fn publish_chat_session_message(
    rabbit_url: &str,
    project_id: i64,
    session_id: i64,
    payload: &[u8],
) -> Result<(), lapin::Error> {
    let (_connection, channel) = ensure_chat_session_exchange(rabbit_url).await?;
    let routing_key = chat_session_routing_key(project_id, session_id);

    channel
        .basic_publish(
            CHAT_SESSION_EXCHANGE,
            &routing_key,
            BasicPublishOptions::default(),
            payload,
            lapin::BasicProperties::default(),
        )
        .await?
        .await?;

    Ok(())
}

pub async fn create_chat_session_consumer(
    rabbit_url: &str,
    project_id: i64,
    session_id: i64,
) -> Result<(Connection, lapin::Consumer), lapin::Error> {
    let (connection, channel) = ensure_chat_session_exchange(rabbit_url).await?;
    let routing_key = chat_session_routing_key(project_id, session_id);

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
            CHAT_SESSION_EXCHANGE,
            &routing_key,
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    let consumer_tag = format!("chat_consumer_{}_{}", project_id, session_id);
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
