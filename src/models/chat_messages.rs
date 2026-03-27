use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use diesel_derive_enum::DbEnum;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

use crate::models::projects::{ProjectReleaseResponse, ProjectUnitResponse};
use crate::services::chat_worker::llm::tools::{
    ToolAnalyzeArchitecturePayload, ToolAnalyzeSolutionPayload, ToolCreateUnitPayload,
    ToolFetchLatestReleasesPayload, ToolFetchRelevantUnitsPayload,
    ToolFinishArchitectureAnalysisPayload, ToolFinishImplementationPayload,
    ToolFinishSolutionAnalysisPayload, ToolImplementSolutionPayload,
    ToolListModulesForUnitContextsPayload, ToolListUnitContextsPayload, ToolModifyUnitPayload,
    ToolRemoveUnitPayload,
};

/// Strips redundant `latest_revision` keys from JSON before persisting `chat_messages` rows.
/// At save time, embedded unit rows already represent a concrete revision; clients can resolve
/// current `latest_revision` via the chat-session unit tips-by-ids endpoint.
pub fn strip_latest_revision_from_stored_json(mut v: Value) -> Value {
    fn walk(x: &mut Value) {
        match x {
            Value::Object(map) => {
                map.remove("latest_revision");
                for child in map.values_mut() {
                    walk(child);
                }
            }
            Value::Array(items) => {
                for item in items.iter_mut() {
                    walk(item);
                }
            }
            _ => {}
        }
    }
    walk(&mut v);
    v
}

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

/// Release row shape for [`ReleaseControllerBoxPayload`] (API name: `ReleaseType`).
pub type ReleaseType = ProjectReleaseResponse;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseControllerBoxPayload {
    pub stage_release: Option<ReleaseType>,
    pub prod_release: Option<ReleaseType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, DbEnum, Serialize, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::ChatSessionRoleType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ChatSessionRoleType {
    User,
    Ai,
    /// Non-model rows (e.g. UI chrome such as [`ChatMessageType::ReleaseControllerBox`]).
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, DbEnum, Serialize, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::ChatMessageType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ChatMessageType {
    Text,
    Units,
    /// UI hint for release/deploy controls after a planner turn that touched units (internal; not LLM input).
    ReleaseControllerBox,
    ToolFetchRelevantUnits,
    ToolFetchLatestReleases,
    ToolListUnitContexts,
    ToolListModulesForUnitContexts,
    ToolAnalyzeArchitecture,
    ToolFinishArchitectureAnalysis,
    ToolAnalyzeSolution,
    ToolFinishSolutionAnalysis,
    ToolImplementSolution,
    ToolFinishImplementation,
    ToolCreateUnit,
    ToolModifyUnit,
    ToolRemoveUnit,
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

/// Chat message body. Serialized to JSON without a type tag (`untagged`), so **never** deserialize
/// from JSON unless you already know the [`ChatMessageType`] — use [`ChatMessagePayload::from_value`]
/// (queue messages and DB rows carry the type separately). Otherwise distinct tools can share the
/// same JSON shape and the wrong enum variant is chosen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChatMessagePayload {
    Text(TextPayload),
    Units(Vec<ProjectUnitResponse>),
    ReleaseControllerBox(ReleaseControllerBoxPayload),
    ToolFetchRelevantUnits(ToolFetchRelevantUnitsPayload),
    ToolFetchLatestReleases(ToolFetchLatestReleasesPayload),
    // `ToolListUnitContexts` / `ToolListModulesForUnitContexts` use permissive `args` + defaults.
    // They MUST come after unit CRUD payloads in this list, otherwise `untagged` deserialize matches
    // them first and steals `create_unit` / `modify_unit` / `remove_unit` JSON (see variant order).
    ToolAnalyzeArchitecture(ToolAnalyzeArchitecturePayload),
    ToolFinishArchitectureAnalysis(ToolFinishArchitectureAnalysisPayload),
    ToolAnalyzeSolution(ToolAnalyzeSolutionPayload),
    ToolFinishSolutionAnalysis(ToolFinishSolutionAnalysisPayload),
    ToolImplementSolution(ToolImplementSolutionPayload),
    ToolFinishImplementation(ToolFinishImplementationPayload),
    ToolCreateUnit(ToolCreateUnitPayload),
    ToolModifyUnit(ToolModifyUnitPayload),
    ToolRemoveUnit(ToolRemoveUnitPayload),
    ToolListUnitContexts(ToolListUnitContextsPayload),
    ToolListModulesForUnitContexts(ToolListModulesForUnitContextsPayload),
}

impl ChatMessagePayload {
    /// Short variant name for logs (no inner data).
    pub fn variant_label(&self) -> &'static str {
        match self {
            ChatMessagePayload::Text(_) => "Text",
            ChatMessagePayload::Units(_) => "Units",
            ChatMessagePayload::ReleaseControllerBox(_) => "ReleaseControllerBox",
            ChatMessagePayload::ToolFetchRelevantUnits(_) => "ToolFetchRelevantUnits",
            ChatMessagePayload::ToolFetchLatestReleases(_) => "ToolFetchLatestReleases",
            ChatMessagePayload::ToolAnalyzeArchitecture(_) => "ToolAnalyzeArchitecture",
            ChatMessagePayload::ToolFinishArchitectureAnalysis(_) => "ToolFinishArchitectureAnalysis",
            ChatMessagePayload::ToolAnalyzeSolution(_) => "ToolAnalyzeSolution",
            ChatMessagePayload::ToolFinishSolutionAnalysis(_) => "ToolFinishSolutionAnalysis",
            ChatMessagePayload::ToolImplementSolution(_) => "ToolImplementSolution",
            ChatMessagePayload::ToolFinishImplementation(_) => "ToolFinishImplementation",
            ChatMessagePayload::ToolListUnitContexts(_) => "ToolListUnitContexts",
            ChatMessagePayload::ToolListModulesForUnitContexts(_) => "ToolListModulesForUnitContexts",
            ChatMessagePayload::ToolCreateUnit(_) => "ToolCreateUnit",
            ChatMessagePayload::ToolModifyUnit(_) => "ToolModifyUnit",
            ChatMessagePayload::ToolRemoveUnit(_) => "ToolRemoveUnit",
        }
    }

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
            ChatMessageType::ReleaseControllerBox => {
                let p = serde_json::from_value::<ReleaseControllerBoxPayload>(v)?;
                ChatMessagePayload::ReleaseControllerBox(p)
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
            ChatMessageType::ToolListUnitContexts => {
                let p = serde_json::from_value::<ToolListUnitContextsPayload>(v)?;
                ChatMessagePayload::ToolListUnitContexts(p)
            }
            ChatMessageType::ToolListModulesForUnitContexts => {
                let p = serde_json::from_value::<ToolListModulesForUnitContextsPayload>(v)?;
                ChatMessagePayload::ToolListModulesForUnitContexts(p)
            }
            ChatMessageType::ToolAnalyzeArchitecture => {
                let p = serde_json::from_value::<ToolAnalyzeArchitecturePayload>(v)?;
                ChatMessagePayload::ToolAnalyzeArchitecture(p)
            }
            ChatMessageType::ToolFinishArchitectureAnalysis => {
                let p = serde_json::from_value::<ToolFinishArchitectureAnalysisPayload>(v)?;
                ChatMessagePayload::ToolFinishArchitectureAnalysis(p)
            }
            ChatMessageType::ToolAnalyzeSolution => {
                let p = serde_json::from_value::<ToolAnalyzeSolutionPayload>(v)?;
                ChatMessagePayload::ToolAnalyzeSolution(p)
            }
            ChatMessageType::ToolFinishSolutionAnalysis => {
                let p = serde_json::from_value::<ToolFinishSolutionAnalysisPayload>(v)?;
                ChatMessagePayload::ToolFinishSolutionAnalysis(p)
            }
            ChatMessageType::ToolImplementSolution => {
                let p = serde_json::from_value::<ToolImplementSolutionPayload>(v)?;
                ChatMessagePayload::ToolImplementSolution(p)
            }
            ChatMessageType::ToolFinishImplementation => {
                let p = serde_json::from_value::<ToolFinishImplementationPayload>(v)?;
                ChatMessagePayload::ToolFinishImplementation(p)
            }
            ChatMessageType::ToolCreateUnit => {
                let p = serde_json::from_value::<ToolCreateUnitPayload>(v)?;
                ChatMessagePayload::ToolCreateUnit(p)
            }
            ChatMessageType::ToolModifyUnit => {
                let p = serde_json::from_value::<ToolModifyUnitPayload>(v)?;
                ChatMessagePayload::ToolModifyUnit(p)
            }
            ChatMessageType::ToolRemoveUnit => {
                let p = serde_json::from_value::<ToolRemoveUnitPayload>(v)?;
                ChatMessagePayload::ToolRemoveUnit(p)
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

/// Parent / provenance for nested chat rows. Conventions:
///
/// - **Planner (or any agent) “root” AI `Text` replies** — `NULL` (not a child of another message).
/// - **Tool rows** (`tool_*`) — parent is the **AI `Text` message** that listed that tool in
///   `requested_tool_calls` (Planner or sub-agent).
/// - **Sub-agent `Text` rows** spawned by async tools (e.g. architecture analyzer) — parent is the
///   **tool message row** that triggered that nested LLM run (so the tree can hang under the tool).
/// - **Further nesting** — tool calls issued inside a sub-agent pass reference that sub-agent’s AI
///   `Text` message id via [`crate::services::chat_worker::processor::process_one`] / tool context
///   (same mechanism as Planner).
///
/// Example chain: `P (planner Text, null) → T (tool, P) → X (analyzer Text, T) → T2 (tool, X)`.
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
    /// Parent link in the tree. On **tool** rows, this is the **AI `Text` message id** that requested
    /// the tool (not the tool row’s id, not necessarily the session root). On **AI `Text`** rows, this
    /// is where that turn hangs (`NULL` = main thread; `Some(tool_id)` = nested under that tool).
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
    /// Same semantics as [`ChatMessageRow::invoking_message_id`].
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
