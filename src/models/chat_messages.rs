use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use diesel_derive_enum::DbEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::projects::ProjectUnitResponse;

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

#[derive(Debug, DbEnum, Serialize, Clone, Deserialize, PartialEq)]
#[ExistingTypePath = "crate::schema::sql_types::ChatMessageStatus"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ChatMessageStatus {
    Generating,
    Failed,
    Stopped,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChatMessagePayload {
    Text(String),
    Unit(ProjectUnitResponse),
    Units(Vec<ProjectUnitResponse>),
    Generating(Option<Value>),
    GeneratingFailed(Option<Value>),
    GeneratingSucceed(Option<Value>),
}

impl ChatMessagePayload {
    pub fn from_value(type_: &ChatMessageType, value: Option<Value>) -> Result<Option<ChatMessagePayload>, serde_json::Error> {
        let Some(v) = value else {
            return Ok(None);
        };
        let payload = match type_ {
            ChatMessageType::Text => {
                let s = serde_json::from_value::<String>(v)?;
                ChatMessagePayload::Text(s)
            }
            ChatMessageType::Unit => {
                let u = serde_json::from_value::<ProjectUnitResponse>(v)?;
                ChatMessagePayload::Unit(u)
            }
            ChatMessageType::Units => {
                let arr = serde_json::from_value::<Vec<ProjectUnitResponse>>(v)?;
                ChatMessagePayload::Units(arr)
            }
            ChatMessageType::Generating => ChatMessagePayload::Generating(Some(v)),
            ChatMessageType::GeneratingFailed => ChatMessagePayload::GeneratingFailed(Some(v)),
            ChatMessageType::GeneratingSucceed => ChatMessagePayload::GeneratingSucceed(Some(v)),
        };
        Ok(Some(payload))
    }

    // pub fn to_value(&self) -> Value {
    //     match self {
    //         ChatMessagePayload::Text(s) => Value::String(s.clone()),
    //         ChatMessagePayload::Unit(u) => serde_json::to_value(u).unwrap_or(Value::Null),
    //         ChatMessagePayload::Units(arr) => serde_json::to_value(arr).unwrap_or(Value::Null),
    //         ChatMessagePayload::Generating(v) => v.clone().unwrap_or(Value::Null),
    //         ChatMessagePayload::GeneratingFailed(v) => v.clone().unwrap_or(Value::Null),
    //         ChatMessagePayload::GeneratingSucceed(v) => v.clone().unwrap_or(Value::Null),
    //     }
    // }
}

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = crate::schema::chat_messages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatMessageRow {
    pub id: i64,
    pub chat_session_id: i64,
    pub role: ChatSessionRoleType,
    #[diesel(column_name = type_)]
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
    pub status: ChatMessageStatus,
    pub created_at: DateTime<Utc>,
}

impl From<ChatMessageRow> for ChatMessageResponse {
    fn from(row: ChatMessageRow) -> Self {
        let payload = ChatMessagePayload::from_value(&row.type_, row.payload).ok().flatten();
        ChatMessageResponse {
            id: row.id,
            chat_session_id: row.chat_session_id,
            role: row.role,
            type_: row.type_,
            payload,
            status: row.status,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ChatMessageResponse {
    pub id: i64,
    pub chat_session_id: i64,
    pub role: ChatSessionRoleType,
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<ChatMessagePayload>,
    pub status: ChatMessageStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct ChatMessageWithFirstResponse {
    #[serde(flatten)]
    pub inner: ChatMessageResponse,
    pub is_first: bool,
}

#[derive(Serialize)]
pub struct CreateChatMessageResponse {
    #[serde(flatten)]
    pub inner: ChatMessageResponse,
    pub is_first: bool,
    pub placeholder_message: ChatMessageResponse,
}

#[derive(Debug, Deserialize)]
pub struct ChatMessageCreateBody {
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
}

impl ChatMessageCreateBody {
    pub fn validate_payload(
        &self,
    ) -> Result<Option<ChatMessagePayload>, serde_json::Error> {
        ChatMessagePayload::from_value(&self.type_, self.payload.clone())
    }
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::chat_messages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatMessageInsert {
    pub chat_session_id: i64,
    pub role: ChatSessionRoleType,
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
    pub status: ChatMessageStatus,
}
