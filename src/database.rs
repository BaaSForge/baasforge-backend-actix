use diesel::prelude::*;
// use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};

// pub type DbPool = Pool<ConnectionManager<PgConnection>>;
// pub type DbConnection = PooledConnection<ConnectionManager<PgConnection>>;

// pub fn establish_pool(database_url: String) -> DbPool {
//   let manager = ConnectionManager::<PgConnection>::new(&database_url);

//   Pool::builder()
//     .test_on_check_out(true)
//     .build(manager)
//     .expect("Failed to create database pool")
// }

// TODO: remove this later and use pool instead
// pub type DbConnection = PgConnection;

#[derive(Clone)]
pub struct DbPool {
  database_url: String,
}

impl DbPool {
  pub fn get(&self) -> Result<PgConnection, diesel::ConnectionError> {
    PgConnection::establish(&self.database_url)
  }
}

pub fn establish_pool(database_url: String) -> DbPool {
  DbPool {
    database_url: database_url,
  }
}
