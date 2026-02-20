use lapin::{BasicProperties, Connection, ConnectionProperties, ExchangeKind, options::*, types::FieldTable};

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
