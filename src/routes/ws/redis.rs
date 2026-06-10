use crate::routes::ws::WsBroker;
use crate::services::redis_conn::{self, RedisConn, CONNECT_TIMEOUT, SUBSCRIBER_RECONNECT_INTERVAL};
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

pub async fn set_stopped_message_on_conn(
    conn: RedisConn,
    message_id: Uuid,
    ttl_secs: u64,
) -> Result<(), redis::RedisError> {
    let mut c = redis_conn::clone_connection(conn);
    set_stopped_message_id(&mut c, message_id, ttl_secs).await
}

/// Check if a message ID is marked as stopped in Redis. Used by chat workers before and during streaming.
pub async fn is_message_stopped<C>(
    conn: &mut C,
    message_id: Uuid,
) -> Result<bool, redis::RedisError>
where
    C: AsyncCommands + Send,
{
    let key = redis_key_stopped_message(message_id);
    let exists: bool = conn.exists(&key).await?;
    Ok(exists)
}

pub async fn is_message_stopped_on_conn(
    conn: RedisConn,
    message_id: Uuid,
) -> Result<bool, redis::RedisError> {
    let mut c = redis_conn::clone_connection(conn);
    is_message_stopped(&mut c, message_id).await
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

pub async fn publish_event_on_conn<E>(
    conn: RedisConn,
    channel: &str,
    event: &E,
) -> Result<(), redis::RedisError>
where
    E: Serialize + ?Sized,
{
    let mut c = redis_conn::clone_connection(conn);
    publish_event_to_channel(&mut c, channel, event).await
}

pub fn run_redis_subscriber(redis_url: String, broker: Arc<WsBroker>) {
    actix_web::rt::spawn(async move {
        let client = match redis_conn::open_client(&redis_url) {
            Ok(client) => client,
            Err(e) => {
                log::error!("[ws-redis] Invalid REDIS_URL: {}", e);
                return;
            }
        };
        loop {
            match run_redis_subscriber_loop(&client, &broker).await {
                Ok(()) => {}
                Err(e) => {
                    log::warn!(
                        "[ws-redis] Subscriber loop error: {}. Reconnecting in {}s.",
                        e,
                        SUBSCRIBER_RECONNECT_INTERVAL.as_secs()
                    );
                    tokio::time::sleep(SUBSCRIBER_RECONNECT_INTERVAL).await;
                }
            }
        }
    });
}

async fn connect_pubsub(
    client: &redis::Client,
) -> Result<redis::aio::PubSub, redis::RedisError> {
    match tokio::time::timeout(CONNECT_TIMEOUT, client.get_async_pubsub()).await {
        Ok(Ok(pubsub)) => Ok(pubsub),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(redis::RedisError::from((
            redis::ErrorKind::Io,
            "pubsub connect timed out",
        ))),
    }
}

async fn run_redis_subscriber_loop(
    client: &redis::Client,
    broker: &WsBroker,
) -> Result<(), redis::RedisError> {
    let mut pubsub = connect_pubsub(client).await?;
    pubsub
        .psubscribe(format!("{}*", USER_CHANNEL_PREFIX))
        .await?;

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
