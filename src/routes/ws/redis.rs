use crate::routes::ws::WsBroker;
use redis::AsyncCommands;
use serde::Serialize;
use std::sync::Arc;
use tokio_stream::StreamExt as _;
use uuid::Uuid;

pub const CHAT_SESSION_CHANNEL_PREFIX: &str = "chat_session:";
pub const USER_CHANNEL_PREFIX: &str = "user:";
/// Redis key prefix for marking a message as stopped (user requested stop generating). Key = prefix + message_id, TTL set by config.
pub const REDIS_KEY_STOPPED_MESSAGE_PREFIX: &str = "chat:stopped_message:";

pub fn redis_key_stopped_message(message_id: Uuid) -> String {
    format!("{}{}", REDIS_KEY_STOPPED_MESSAGE_PREFIX, message_id)
}

/// Mark a message ID as stopped in Redis with the given TTL (seconds). Used by the stop-generating endpoint.
pub async fn set_stopped_message_id<C>(
    conn: &mut C,
    message_id: Uuid,
    ttl_secs: u64,
) -> Result<(), redis::RedisError>
where
    C: AsyncCommands + Send,
{
    let key = redis_key_stopped_message(message_id);
    conn.set_ex::<_, _, ()>(key, "1", ttl_secs).await
}

/// Check if a message ID is marked as stopped in Redis. Used by chat workers before and during streaming.
pub async fn is_message_stopped<C>(conn: &mut C, message_id: Uuid) -> Result<bool, redis::RedisError>
where
    C: AsyncCommands + Send,
{
    let key = redis_key_stopped_message(message_id);
    let exists: bool = conn.exists(&key).await?;
    Ok(exists)
}

pub fn user_channel(user_id: Uuid) -> String {
    format!("{}{}", USER_CHANNEL_PREFIX, user_id)
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
    // Single channel pattern: all events (chat, project deleted, runtime created) are published to user:{user_id}.
    pubsub.psubscribe(format!("{}*", USER_CHANNEL_PREFIX)).await?;

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
