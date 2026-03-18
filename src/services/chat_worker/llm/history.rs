//! Upstream message types and conversion from session history.
//!
//! LLM APIs expect a strict order: User → Assistant (with tool_calls) → Tool result for each
//! tool_call_id → … The tool-call request (assistant message) and each tool result are always
//! separate messages; we never combine them into one.

use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageType, ChatSessionRoleType, TextPayload,
};
use serde_json::Value;

/// One tool call from an assistant message (id, name, arguments).
/// thought_signature is required by Gemini when replaying history.
#[derive(Debug, Clone)]
pub struct UpstreamToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    /// Gemini thoughtSignature; required when sending function call parts back to Gemini.
    pub thought_signature: Option<String>,
}

/// One entry in the conversation history we send upstream.
#[derive(Debug, Clone)]
pub enum UpstreamHistoryEntry {
    User { text: String },
    Assistant {
        text: String,
        tool_calls: Option<Vec<UpstreamToolCall>>,
    },
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        content: String,
    },
}

/// Tool message types that represent tool execution results (to be sent as tool/tool_result).
fn is_tool_result_type(type_: &ChatMessageType) -> bool {
    matches!(
        type_,
        ChatMessageType::ToolFetchRelevantUnits | ChatMessageType::ToolFetchLatestReleases
    )
}

fn parse_text_payload(payload: &Option<Value>) -> Option<TextPayload> {
    payload
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Extract tool_call_id, tool_name, and result content from a tool-result payload.
fn tool_result_from_payload(
    type_: &ChatMessageType,
    payload: &Option<Value>,
    fallback_id: &mut usize,
) -> (String, String, String) {
    let typed = ChatMessagePayload::from_value(type_, payload.clone()).ok().flatten();
    let (tool_use_id_opt, tool_name_opt, content) = match &typed {
        Some(ChatMessagePayload::ToolFetchRelevantUnits(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFetchLatestReleases(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        _ => (
            None,
            None,
            payload
                .as_ref()
                .map(|p| serde_json::to_string(p).unwrap_or_default())
                .unwrap_or_default(),
        ),
    };
    let tool_use_id = tool_use_id_opt.unwrap_or_else(|| {
        let id = format!("tool_{}", *fallback_id);
        *fallback_id += 1;
        id
    });
    let tool_name = tool_name_opt.unwrap_or_default();
    (tool_use_id, tool_name, content)
}

fn payload_to_string(payload: &Option<Value>) -> String {
    match payload {
        None => String::new(),
        Some(v) => v
            .get("text")
            .and_then(|t| t.as_str())
            .map(String::from)
            .unwrap_or_else(|| v.to_string()),
    }
}

/// Convert session history + current user text into a typed sequence of upstream entries.
/// Order is exactly the order of session_messages (DB order): we iterate once and push one entry per message.
pub fn session_messages_to_upstream_entries(
    session_messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    user_text: &str,
) -> Vec<UpstreamHistoryEntry> {
    let mut out = Vec::new();
    let mut fallback_tool_id = 0usize;

    for (role, type_, payload) in session_messages {
        match (role, type_) {
            (ChatSessionRoleType::User, _) => {
                let text = payload_to_string(payload).trim().to_string();
                if !text.is_empty() {
                    out.push(UpstreamHistoryEntry::User { text });
                }
            }
            (ChatSessionRoleType::Ai, msg_type) if is_tool_result_type(msg_type) => {
                let (tool_use_id, tool_name, content) =
                    tool_result_from_payload(msg_type, payload, &mut fallback_tool_id);
                out.push(UpstreamHistoryEntry::ToolResult {
                    tool_call_id: tool_use_id,
                    tool_name,
                    content,
                });
            }
            (ChatSessionRoleType::Ai, ChatMessageType::Text) => {
                if let Some(tp) = parse_text_payload(payload) {
                    let text = tp.text.trim().to_string();
                    let tool_calls: Option<Vec<UpstreamToolCall>> = tp
                        .requested_tool_calls
                        .as_ref()
                        .filter(|c| !c.is_empty())
                        .map(|c| {
                            c.iter()
                                .map(|tc| {
                                    let id = tc
                                        .tool_call_id
                                        .clone()
                                        .unwrap_or_else(|| {
                                            let id = format!("tool_{}", fallback_tool_id);
                                            fallback_tool_id += 1;
                                            id
                                        });
                                    UpstreamToolCall {
                                        id,
                                        name: tc.tool_name.clone(),
                                        arguments: tc.tool_arguments_to_pass.clone(),
                                        thought_signature: tc.thought_signature.clone(),
                                    }
                                })
                                .collect()
                        });
                    if !text.is_empty() || tool_calls.is_some() {
                        out.push(UpstreamHistoryEntry::Assistant { text, tool_calls });
                    }
                }
            }
            _ => {
                let text = payload_to_string(payload).trim().to_string();
                if !text.is_empty() {
                    let entry = match role {
                        ChatSessionRoleType::User => UpstreamHistoryEntry::User { text },
                        ChatSessionRoleType::Ai => UpstreamHistoryEntry::Assistant {
                            text,
                            tool_calls: None,
                        },
                    };
                    out.push(entry);
                }
            }
        }
    }

    if !user_text.trim().is_empty() {
        out.push(UpstreamHistoryEntry::User {
            text: user_text.trim().to_string(),
        });
    }

    out
}
