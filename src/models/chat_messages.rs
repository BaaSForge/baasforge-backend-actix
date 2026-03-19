use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use diesel_derive_enum::DbEnum;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

use crate::models::projects::ProjectUnitResponse;
use crate::services::chat_worker::llm::tools::{
    ToolFetchLatestReleasesPayload, ToolFetchRelevantUnitsPayload,
};

/// One requested tool call from the LLM (GPT tool_call_id optional; Gemini has no id).
/// For Gemini, thought_signature must be stored and sent back in history for tool calls to work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestedToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub tool_name: String,
    #[serde(alias = "tool_arguments_to_pass")]
    pub tool_arguments_to_pass: Value,
    /// Gemini: required when replaying function calls in history (thoughtSignature).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

fn serialize_signature_for_db<S>(v: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match v {
        None => serializer.serialize_none(),
        Some(s) if s.is_empty() => serializer.serialize_none(),
        Some(s) => serializer.serialize_str(s),
    }
}

/// One extended-thinking segment: visible `thinking` text plus optional opaque `signature` (Anthropic replay).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThinkingBlock {
    pub thinking: String,
    /// Opaque Anthropic replay token; must be sent back with `thinking` in message history.
    /// Always written to JSON as `null` or a string so the key is never dropped on save.
    #[serde(default, serialize_with = "serialize_signature_for_db")]
    pub signature: Option<String>,
}

/// Payload for Text messages: streamed text and optional requested tool calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TextPayload {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub requested_tool_calls: Option<Vec<RequestedToolCall>>,
    /// Reasoning segments (e.g. Claude extended thinking).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thinking_blocks: Option<Vec<ThinkingBlock>>,
}

impl TextPayload {
    /// Concatenated visible thinking for clients (WS/API) without storing a duplicate column.
    pub fn thinking_display(&self) -> Option<String> {
        let blocks = self.thinking_blocks.as_ref()?;
        let mut out = String::new();
        for b in blocks {
            out.push_str(&b.thinking);
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    pub fn has_thinking_stored(&self) -> bool {
        self.thinking_blocks.as_ref().is_some_and(|b| {
            b.iter().any(|blk| {
                !blk.thinking.is_empty()
                    || blk
                        .signature
                        .as_ref()
                        .is_some_and(|s| !s.is_empty())
            })
        })
    }
}

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
    ToolFetchRelevantUnits,
    ToolFetchLatestReleases,
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

#[derive(Debug, Default, DbEnum, Serialize, Clone, Deserialize, PartialEq)]
#[ExistingTypePath = "crate::schema::sql_types::ChatAgentType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ChatAgentType {
    #[default]
    Planner,
    ArchitectureAnalyzer,
    SolutionAnalyzer,
    Developer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChatMessagePayload {
    Text(TextPayload),
    Units(Vec<ProjectUnitResponse>),
    ToolFetchRelevantUnits(ToolFetchRelevantUnitsPayload),
    ToolFetchLatestReleases(ToolFetchLatestReleasesPayload),
}

impl ChatMessagePayload {
    pub fn from_value(
        type_: &ChatMessageType,
        value: Option<Value>,
    ) -> Result<Option<ChatMessagePayload>, serde_json::Error> {
        let Some(v) = value else {
            return Ok(None);
        };
        let payload = match type_ {
            ChatMessageType::Text => {
                let text_payload = serde_json::from_value::<TextPayload>(v)?;
                ChatMessagePayload::Text(text_payload)
            }
            ChatMessageType::Units => {
                let arr = serde_json::from_value::<Vec<ProjectUnitResponse>>(v)?;
                ChatMessagePayload::Units(arr)
            }
            ChatMessageType::ToolFetchRelevantUnits => {
                let p = serde_json::from_value::<ToolFetchRelevantUnitsPayload>(v)?;
                ChatMessagePayload::ToolFetchRelevantUnits(p)
            }
            ChatMessageType::ToolFetchLatestReleases => {
                let p = serde_json::from_value::<ToolFetchLatestReleasesPayload>(v)?;
                ChatMessagePayload::ToolFetchLatestReleases(p)
            }
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
    pub id: Uuid,
    pub chat_session_id: Uuid,
    pub role: ChatSessionRoleType,
    #[diesel(column_name = type_)]
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
    pub status: ChatMessageStatus,
    pub created_at: DateTime<Utc>,
    pub invoking_message_id: Option<Uuid>,
    /// Set only for AI role messages; NULL for user messages.
    pub agent: Option<ChatAgentType>,
}

impl From<ChatMessageRow> for ChatMessageResponse {
    fn from(row: ChatMessageRow) -> Self {
        let payload = ChatMessagePayload::from_value(&row.type_, row.payload)
            .ok()
            .flatten();
        ChatMessageResponse {
            id: row.id,
            chat_session_id: row.chat_session_id,
            role: row.role,
            type_: row.type_,
            payload,
            status: row.status,
            created_at: row.created_at,
            invoking_message_id: row.invoking_message_id,
            agent: row.agent,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessageResponse {
    pub id: Uuid,
    pub chat_session_id: Uuid,
    pub role: ChatSessionRoleType,
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<ChatMessagePayload>,
    pub status: ChatMessageStatus,
    pub created_at: DateTime<Utc>,
    pub invoking_message_id: Option<Uuid>,
    /// Present only for AI role messages.
    pub agent: Option<ChatAgentType>,
}

#[derive(Serialize)]
pub struct ChatMessageWithFirstResponse {
    #[serde(flatten)]
    pub inner: ChatMessageResponse,
    pub is_first: bool,
}

#[derive(Debug, Deserialize)]
pub struct ChatMessageCreateBody {
    #[serde(rename = "type")]
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
}

impl ChatMessageCreateBody {
    pub fn validate_payload(&self) -> Result<Option<ChatMessagePayload>, serde_json::Error> {
        ChatMessagePayload::from_value(&self.type_, self.payload.clone())
    }
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::chat_messages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatMessageInsert {
    pub chat_session_id: Uuid,
    pub role: ChatSessionRoleType,
    pub type_: ChatMessageType,
    pub payload: Option<Value>,
    pub status: ChatMessageStatus,
    /// Set only for AI role; None for user messages.
    pub agent: Option<ChatAgentType>,
}
