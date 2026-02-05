use lapin::{Connection, ConnectionProperties, options::QueueDeclareOptions, types::FieldTable};

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
