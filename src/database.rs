//! When running multiple replicas (e.g. 30 API + 30 workers), each process has its own pool.
//! Total PostgreSQL connections ≤ (API_replicas + worker_replicas) × max_size. Set max_size
//! so this stays under PostgreSQL `max_connections` (or put PgBouncer in front of Postgres).
//!
//! All DB access from async handlers/middlewares must go through `block_on_db` so blocking
//! r2d2/Diesel I/O does not block the actix worker thread (which causes 408 timeouts).
//!
//! After a long PostgreSQL outage, `test_on_check_out` drops stale connections and r2d2 opens
//! new ones on the next `pool.get()`. `idle_timeout` evicts idle sockets; r2d2's default
//! `max_lifetime` (30 min) rotates connections. `min_idle(0)` avoids proactively opening
//! `max_size` connections in the background (r2d2's default when `min_idle` is unset).
//!
//! Connections are never held across `.await` — only inside `block_on_db` / `block_on_db_tokio`
//! closures, so they are always returned to the pool when the closure ends (or discarded if broken).

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use std::time::Duration;

pub type DbPool = Pool<ConnectionManager<PgConnection>>;

pub type PooledConn = diesel::r2d2::PooledConnection<ConnectionManager<PgConnection>>;

#[derive(Debug)]
pub enum BlockingDbError {
    Pool(r2d2::Error),
    Blocking,
    Query(diesel::result::Error),
}

impl std::fmt::Display for BlockingDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockingDbError::Pool(e) => write!(f, "pool: {}", e),
            BlockingDbError::Blocking => write!(f, "blocking task cancelled"),
            BlockingDbError::Query(e) => write!(f, "query: {}", e),
        }
    }
}

impl std::error::Error for BlockingDbError {}

impl From<r2d2::Error> for BlockingDbError {
    fn from(e: r2d2::Error) -> Self {
        BlockingDbError::Pool(e)
    }
}

impl From<diesel::result::Error> for BlockingDbError {
    fn from(e: diesel::result::Error) -> Self {
        BlockingDbError::Query(e)
    }
}

fn run_on_pool<F, R>(pool: &DbPool, f: F) -> Result<R, BlockingDbError>
where
    F: FnOnce(&mut PooledConn) -> Result<R, diesel::result::Error>,
{
    let mut conn = pool.get().map_err(BlockingDbError::Pool)?;
    f(&mut conn).map_err(BlockingDbError::Query)
}

/// Runs blocking DB work on a dedicated thread so the async worker is not blocked.
/// Use this for all pool.get() + Diesel work in actix handlers and middlewares to avoid 408 timeouts.
pub async fn block_on_db<F, R>(pool: &DbPool, f: F) -> Result<R, BlockingDbError>
where
    F: FnOnce(&mut PooledConn) -> Result<R, diesel::result::Error> + Send + 'static,
    R: Send + 'static,
{
    let pool = pool.clone();
    match actix_web::web::block(move || run_on_pool(&pool, f)).await {
        Ok(r) => r,
        Err(_) => Err(BlockingDbError::Blocking),
    }
}

/// Same as `block_on_db` but for tokio runtimes (e.g. the chat_worker binary).
/// Use this in async code that runs on tokio so blocking r2d2/Diesel work doesn't block worker threads.
pub async fn block_on_db_tokio<F, R>(pool: &DbPool, f: F) -> Result<R, BlockingDbError>
where
    F: FnOnce(&mut PooledConn) -> Result<R, diesel::result::Error> + Send + 'static,
    R: Send + 'static,
{
    let pool = pool.clone();
    match tokio::task::spawn_blocking(move || run_on_pool(&pool, f)).await {
        Ok(r) => r,
        Err(_) => Err(BlockingDbError::Blocking),
    }
}

pub fn establish_pool(
    database_url: String,
    max_size: u32,
    connection_timeout_secs: u64,
    idle_timeout_secs: u64,
) -> DbPool {
    let manager = ConnectionManager::<PgConnection>::new(&database_url);

    let idle_timeout = if idle_timeout_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(idle_timeout_secs))
    };

    let pool = Pool::builder()
        .max_size(max_size)
        .min_idle(Some(0))
        .connection_timeout(Duration::from_secs(connection_timeout_secs))
        .idle_timeout(idle_timeout)
        .test_on_check_out(true)
        .build(manager)
        .expect("Failed to create database pool");

    if let Err(e) = validate_pool_connection(&pool) {
        panic!(
            "Database connection failed at startup.\n\
             Check: DATABASE_URL is correct, PostgreSQL is running on the given host:port, and the user/password have access.\n\
             If running in Docker, use host.docker.internal or the service name instead of localhost.\n\
             Error: {:?}\nDisplay: {}",
            e, e
        );
    }

    log::info!("Database pool connected successfully");
    pool
}

fn validate_pool_connection(pool: &DbPool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut conn = pool.get().map_err(|e| format!("pool get: {:?}", e))?;
    diesel::sql_query("SELECT 1")
        .execute(&mut conn)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    Ok(())
}
