use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::WsBroker;
use redis::AsyncCommands;
use std::sync::Arc;
use tokio_stream::StreamExt as _;

/// Redis channel name prefix for chat session streams. Channel = topic (e.g. `chat_session:1:5:10`).
pub const CHAT_SESSION_CHANNEL_PREFIX: &str = "chat_session:";

pub async fn publish_chat_chunk(
    redis_url: &str,
    topic: &str,
    payload: ChatSessionNewMessageChunkPayload,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut conn = client.get_multiplexed_async_connection().await?;
    let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
    let msg = serde_json::to_string(&event).map_err(|e| {
        redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "serialize", e.to_string()))
    })?;
    conn.publish::<_, _, ()>(topic, msg).await?;
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
    pubsub.psubscribe(format!("{}*", CHAT_SESSION_CHANNEL_PREFIX)).await?;

    let mut stream = pubsub.on_message();

    while let Some(msg) = stream.next().await {
        let channel: String = msg.get_channel_name().to_string();
        let payload: String = msg.get_payload().map_err(|e| {
            redis::RedisError::from((redis::ErrorKind::InvalidClientConfig, "payload", e.to_string()))
        })?;
        broker.broadcast_to_topic(&channel, &payload).await;
    }

    Ok(())
}
