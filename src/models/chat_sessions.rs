use chrono::{DateTime, Utc};
use diesel::Queryable;
use diesel::Selectable;
use diesel_derive_enum::DbEnum;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::ChatSessionRoleType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ChatSessionRoleType {
    User,
    Ai,
}

#[derive(Debug, DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::ChatMessageType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ChatMessageType {
    Text,
    Units,
    Unit,
    Generating,
    GeneratingFailed,
    GeneratingSucceed,
}

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::chat_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatSessionResponse {
    pub id: i64,
    pub project_id: i64,
    pub user_id: i64,
    pub title: String,
    pub role: ChatSessionRoleType,
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
    pub created_at: DateTime<Utc>,
}
