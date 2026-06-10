use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::Client;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Shared handle; `ConnectionManager` is internally `Arc`-backed and cheap to clone per command.
pub type RedisConnection = ConnectionManager;
pub type RedisConn = Arc<RedisConnection>;
pub type SharedRedisConn = Option<RedisConn>;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SUBSCRIBER_RECONNECT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum RedisConnectError {
    #[error("redis connect timed out after {0}s")]
    Timeout(u64),
    #[error("redis connect failed: {0}")]
    Redis(#[from] redis::RedisError),
}

pub fn connection_manager_config() -> ConnectionManagerConfig {
    ConnectionManagerConfig::new()
        .set_number_of_retries(2)
        .set_min_delay(Duration::from_millis(500))
        .set_max_delay(Duration::from_secs(2))
        .set_exponent_base(2.0)
        .set_connection_timeout(Some(Duration::from_secs(5)))
        .set_response_timeout(Some(CONNECT_TIMEOUT))
}

pub fn open_client(redis_url: &str) -> redis::RedisResult<Client> {
    Client::open(redis_url)
}

async fn connect_manager(client: &Client) -> redis::RedisResult<ConnectionManager> {
    client
        .get_connection_manager_with_config(connection_manager_config())
        .await
}

pub async fn connect_redis_conn(redis_url: &str) -> Result<RedisConn, RedisConnectError> {
    let client = open_client(redis_url).map_err(RedisConnectError::Redis)?;
    match tokio::time::timeout(CONNECT_TIMEOUT, connect_manager(&client)).await {
        Ok(Ok(manager)) => Ok(Arc::new(manager)),
        Ok(Err(e)) => Err(RedisConnectError::Redis(e)),
        Err(_) => Err(RedisConnectError::Timeout(CONNECT_TIMEOUT.as_secs())),
    }
}

pub fn clone_connection(conn: RedisConn) -> RedisConnection {
    conn.as_ref().clone()
}
