use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use validator::Validate;
use chrono::{DateTime, Utc};

use super::schema::*;

#[derive(Queryable, Serialize, Deserialize, Validate, Clone)]
pub struct User {
  pub id: Int8,
  pub platform: PlatformType,
  pub id_in_platform: String,
  pub full_name: Option<String>,
}

#[derive(Insertable, Deserialize, Validate, Clone)]
pub struct NewUser {
  #[serde(skip_serializing)]
  pub id: Int8,
  pub platform: PlatformType,
  pub id_in_platform: String,
  pub full_name: Option<String>,
}

impl User {
  pub fn display_name(&self) -> String {
    match &self.full_name {
      Some(name) => name.clone(),
      None => "No Name".to_string(),
    }
  }

  pub fn formatted_created_at(date: DateTime<Utc>) -> String {
    date.to_rfc3339()
  }
}
