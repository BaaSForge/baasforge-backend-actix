use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};

pub type DbPool = Pool<ConnectionManager<PgConnection>>;

pub fn establish_pool(database_url: String) -> DbPool {
  let manager = ConnectionManager::<PgConnection>::new(&database_url);

  Pool::builder()
    .test_on_check_out(true)
    .build(manager)
    .expect("Failed to create database pool")
}
