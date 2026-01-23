use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;

#[derive(Debug, diesel_derive_enum::DbEnum)]
#[ExistingTypePath = "crate::schema::sql_types::UserTokenType"]
#[DbValueStyle = "snake_case"]
pub enum UserTokenType {
    Refresh,
    Access,
}

#[derive(Debug, Queryable, Insertable)]
#[diesel(table_name = crate::schema::user_tokens)]
pub struct UserToken {
    pub user_id: i64,
    pub token_type: UserTokenType,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub revoked: Option<bool>,
}
