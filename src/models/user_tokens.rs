use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, diesel_derive_enum::DbEnum, Serialize, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::UserTokenType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum UserTokenType {
    Refresh,
    Access,
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = crate::schema::user_tokens)]
pub struct UserToken {
    pub id: Uuid,
    pub user_id: Uuid,
    #[allow(dead_code)]
    pub token_type: UserTokenType,
    #[allow(dead_code)]
    pub token: String,
    #[allow(dead_code)]
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub revoked: Option<bool>,
    pub is_signup_token: bool,
    #[allow(dead_code)]
    pub refresh_token_id: Option<Uuid>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::user_tokens)]
pub struct UserTokenInternal {
    pub user_id: Uuid,
    pub token_type: UserTokenType,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub revoked: Option<bool>,
    pub is_signup_token: bool,
    pub refresh_token_id: Option<Uuid>,
}

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::user_tokens)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct UserTokenResponse {
    pub id: Uuid,
    pub token_type: UserTokenType,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub revoked: Option<bool>,
    pub is_signup_token: bool,
    pub refresh_token_id: Option<Uuid>,
}
