use chrono::{DateTime, Utc};
use diesel::Insertable;
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

#[derive(Debug, DbEnum, Serialize, Clone, Deserialize, PartialEq)]
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
#[diesel(table_name = crate::schema::chat_messages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatMessageResponse {
    pub id: i64,
    pub chat_session_id: i64,
    pub role: ChatSessionRoleType,
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ChatMessageCreateBody {
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::chat_messages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatMessageInsert {
    pub chat_session_id: i64,
    pub role: ChatSessionRoleType,
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
}
