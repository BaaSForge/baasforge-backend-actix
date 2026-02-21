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

    Pool::builder()
        .max_size(max_size)
        .connection_timeout(Duration::from_secs(connection_timeout_secs))
        .test_on_check_out(true)
        .build(manager)
        .expect("Failed to create database pool")
}
