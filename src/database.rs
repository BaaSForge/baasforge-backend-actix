//! When running multiple replicas (e.g. 30 API + 30 workers), each process has its own pool.
//! Total PostgreSQL connections ≤ (API_replicas + worker_replicas) × max_size. Set max_size
//! so this stays under PostgreSQL `max_connections` (or put PgBouncer in front of Postgres).

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use std::time::Duration;

pub type DbPool = Pool<ConnectionManager<PgConnection>>;

pub fn establish_pool(
    database_url: String,
    max_size: u32,
    connection_timeout_secs: u64,
) -> DbPool {
    let manager = ConnectionManager::<PgConnection>::new(&database_url);

    let pool = Pool::builder()
        .max_size(max_size)
        .connection_timeout(Duration::from_secs(connection_timeout_secs))
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
