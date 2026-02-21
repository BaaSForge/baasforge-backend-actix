use crate::routes::ws::WsBroker;
use redis::AsyncCommands;
use serde::Serialize;
use std::sync::Arc;
use tokio_stream::StreamExt as _;

pub const CHAT_SESSION_CHANNEL_PREFIX: &str = "chat_session:";
pub const PROJECT_CHANNEL_PREFIX: &str = "project:";

pub fn project_broadcast_channel(project_id: i64) -> String {
    format!("{}{}", PROJECT_CHANNEL_PREFIX, project_id)
}

pub async fn publish_event_to_channel<C, E>(
    conn: &mut C,
    channel: &str,
    event: &E,
) -> Result<(), redis::RedisError>
where
    C: AsyncCommands + Send,
    E: Serialize + ?Sized,
{
    let msg = serde_json::to_string(event).map_err(|e| {
        redis::RedisError::from((
            redis::ErrorKind::InvalidClientConfig,
            "serialize",
            e.to_string(),
        ))
    })?;
    conn.publish::<_, _, ()>(channel, msg).await?;
    Ok(())
}

pub fn run_redis_subscriber(redis_url: String, broker: Arc<WsBroker>) {
    actix_web::rt::spawn(async move {
        loop {
            match run_redis_subscriber_loop(&redis_url, &broker).await {
                Ok(()) => {}
                Err(e) => {
                    log::warn!("[ws-redis] Subscriber loop error: {}. Reconnecting in 5s.", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    });
}

async fn run_redis_subscriber_loop(
    redis_url: &str,
    broker: &WsBroker,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut pubsub = client.get_async_pubsub().await?;
    // One connection can subscribe to many patterns; Redis delivers all matching messages on the same stream.
    // Adding more psubscribe calls here is cheap (one command per pattern at startup) and does not require separate loops.
    pubsub.psubscribe(format!("{}*", CHAT_SESSION_CHANNEL_PREFIX)).await?;
    pubsub.psubscribe(format!("{}*", PROJECT_CHANNEL_PREFIX)).await?;

    let mut stream = pubsub.on_message();

    while let Some(msg) = stream.next().await {
        let channel: String = msg.get_channel_name().to_string();
        let payload: String = msg.get_payload().map_err(|e| {
            redis::RedisError::from((
                redis::ErrorKind::InvalidClientConfig,
                "payload",
                e.to_string(),
            ))
        })?;
        broker.broadcast_to_topic(&channel, &payload).await;
    }

    Ok(())
}
