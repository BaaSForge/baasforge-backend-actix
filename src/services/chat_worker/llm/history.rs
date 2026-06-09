//! Upstream message types and conversion from session history.
//!
//! LLM APIs expect a strict order: User → Assistant (with tool_calls) → Tool result for each
//! tool_call_id → … The tool-call request (assistant message) and each tool result are always
//! separate messages; we never combine them into one.
//!
//! Types listed in [`UPSTREAM_EXCLUDED_MESSAGE_TYPES`] are stripped in
//! [`filter_session_messages_for_upstream_llm`] (called from [`crate::services::chat_worker::agent::load_history_for_agent`])
//! and skipped again in [`session_messages_to_upstream_entries`] as a safeguard.
//! [`ChatMessageType::RejectControllerBox`] is included upstream as [`UpstreamHistoryEntry::System`]:
//! rejection notice when `is_rejected` is true, or implicit-acceptance notice when it is false.

use crate::libs::baal_inline::expand_baal_inlines_for_llm_user_text;
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageType, ChatSessionRoleType, TextPayload, ThinkingBlock,
};
use crate::services::chat_worker::llm::tools::get_package_type_definition_file_contents;
use crate::services::chat_worker::llm::tools::get_packages_type_definition_files;
use serde_json::{Value, json};
use std::collections::HashSet;

/// [`ChatMessageType`] values that must never be sent to LLM APIs (UI/session metadata only).
/// Extend this list when adding similar non-model rows.
pub const UPSTREAM_EXCLUDED_MESSAGE_TYPES: &[ChatMessageType] =
    &[ChatMessageType::ReleaseControllerBox];

#[inline]
pub fn is_excluded_from_upstream_llm_history(type_: &ChatMessageType) -> bool {
    UPSTREAM_EXCLUDED_MESSAGE_TYPES.contains(type_)
}

/// Removes excluded types before [`session_messages_to_upstream_entries`], tool-result cleanup, and trimming.
pub fn filter_session_messages_for_upstream_llm(
    messages: Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>,
) -> Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)> {
    messages
        .into_iter()
        .filter(|(_, t, _)| !is_excluded_from_upstream_llm_history(t))
        .collect()
}

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
    User {
        text: String,
    },
    Assistant {
        text: String,
        tool_calls: Option<Vec<UpstreamToolCall>>,
        /// Extended-thinking blocks for replay (Claude extended thinking; Gemini thought summaries + signatures).
        thinking_blocks: Option<Vec<ThinkingBlock>>,
    },
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        content: String,
    },
    /// Instructional notice (e.g. reject-controller); mapped to provider-specific roles (`system` or `user`).
    System {
        text: String,
    },
}

/// Tool message types that represent tool execution results (to be sent as tool/tool_result).
fn is_tool_result_type(type_: &ChatMessageType) -> bool {
    matches!(
        type_,
        ChatMessageType::ToolFetchRelevantUnits
            | ChatMessageType::ToolFetchSpecificProductionReleaseUnits
            | ChatMessageType::ToolFetchLatestSpecificProductionReleaseUnits
            | ChatMessageType::ToolFetchLatestReleases
            | ChatMessageType::ToolFetchReleaseTraces
            | ChatMessageType::ToolListUnitContexts
            | ChatMessageType::ToolListModulesForUnitContexts
            | ChatMessageType::ToolListProjectEnvKeys
            | ChatMessageType::ToolGetPackagesTypeDefinitionFiles
            | ChatMessageType::ToolGetPackageTypeDefinitionFileContents
            | ChatMessageType::ToolAnalyzeArchitecture
            | ChatMessageType::ToolFinishArchitectureAnalysis
            | ChatMessageType::ToolAnalyzeSolution
            | ChatMessageType::ToolFinishSolutionAnalysis
            | ChatMessageType::ToolAnalyzeObservability
            | ChatMessageType::ToolFinishObservabilityAnalysis
            | ChatMessageType::ToolImplementSolution
            | ChatMessageType::ToolFinishImplementation
            | ChatMessageType::ToolCreateUnit
            | ChatMessageType::ToolModifyUnit
            | ChatMessageType::ToolRemoveUnit
    )
}

fn parse_text_payload(payload: &Option<Value>) -> Option<TextPayload> {
    payload
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// If the tool row JSON has a top-level `error` string (including pre-queue failure shapes stored in
/// the DB `payload` column), wrap the tool result in a small JSON object with `tool_error` plus
/// `details`. **Must stay valid JSON** — Gemini replays use `serde_json::from_str` on tool responses.
fn emphasize_tool_error_for_upstream(payload: &Option<Value>, content: String) -> String {
    let Some(err) = payload.as_ref().and_then(|v| {
        v.get("error")
            .and_then(|e| e.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    }) else {
        return content;
    };
    let details = match serde_json::from_str::<Value>(&content) {
        Ok(v) => v,
        Err(_) => Value::String(content.clone()),
    };
    serde_json::to_string(&json!({
        "tool_error": err,
        "details": details,
    }))
    .unwrap_or(content)
}

/// Remove internal sub-task status fields from a unit tool payload before sending to the LLM.
/// These fields are UI-only and must not appear in LLM history.
fn strip_unit_subtask_fields(mut v: Value) -> String {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("pre_check_sub_task");
        obj.remove("compiler_check_sub_task");
        obj.remove("lint_check_sub_task");
        obj.remove("rule_check_sub_task");
    }
    v.to_string()
}

/// Extract tool_call_id, tool_name, and result content from a tool-result payload.
///
/// `hydrate_static_tool_results`: when false, callers only need ids/names (history cleanup passes).
/// When true, static manifests are expanded for upstream LLM tool results (e.g.
/// [`get_packages_type_definition_files`] file paths and
/// [`get_package_type_definition_file_contents`] file contents — same idea as [`strip_unit_subtask_fields`]).
fn tool_result_from_payload(
    type_: &ChatMessageType,
    payload: &Option<Value>,
    fallback_id: &mut usize,
    hydrate_static_tool_results: bool,
) -> (String, String, String) {
    let typed = ChatMessagePayload::from_value(type_, payload.clone())
        .ok()
        .flatten();
    let (tool_use_id_opt, tool_name_opt, content) = match &typed {
        Some(ChatMessagePayload::ToolFetchRelevantUnits(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFetchSpecificProductionReleaseUnits(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFetchLatestSpecificProductionReleaseUnits(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFetchLatestReleases(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFetchReleaseTraces(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolListUnitContexts(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolListModulesForUnitContexts(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolListProjectEnvKeys(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolGetPackagesTypeDefinitionFiles(p)) => {
            let content = if hydrate_static_tool_results {
                get_packages_type_definition_files::upstream_tool_result_content(p)
            } else {
                serde_json::to_string(p).unwrap_or_default()
            };
            (p.tool_call_id.clone(), p.tool_name.clone(), content)
        }
        Some(ChatMessagePayload::ToolGetPackageTypeDefinitionFileContents(p)) => {
            let content = if hydrate_static_tool_results {
                get_package_type_definition_file_contents::upstream_tool_result_content(p)
            } else {
                serde_json::to_string(p).unwrap_or_default()
            };
            (p.tool_call_id.clone(), p.tool_name.clone(), content)
        }
        Some(ChatMessagePayload::ToolAnalyzeArchitecture(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFinishArchitectureAnalysis(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolAnalyzeSolution(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFinishSolutionAnalysis(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolAnalyzeObservability(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFinishObservabilityAnalysis(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolImplementSolution(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolFinishImplementation(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            serde_json::to_string(p).unwrap_or_default(),
        ),
        Some(ChatMessagePayload::ToolCreateUnit(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            strip_unit_subtask_fields(serde_json::to_value(p).unwrap_or_default()),
        ),
        Some(ChatMessagePayload::ToolModifyUnit(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            strip_unit_subtask_fields(serde_json::to_value(p).unwrap_or_default()),
        ),
        Some(ChatMessagePayload::ToolRemoveUnit(p)) => (
            p.tool_call_id.clone(),
            p.tool_name.clone(),
            strip_unit_subtask_fields(serde_json::to_value(p).unwrap_or_default()),
        ),
        _ => {
            let (loose_id, loose_name) = if is_tool_result_type(type_) {
                payload
                    .as_ref()
                    .map(|v| {
                        (
                            v.get("tool_call_id")
                                .and_then(|x| x.as_str())
                                .map(String::from),
                            v.get("tool_name")
                                .and_then(|x| x.as_str())
                                .map(String::from),
                        )
                    })
                    .unwrap_or((None, None))
            } else {
                (None, None)
            };
            let content = payload
                .as_ref()
                .map(|p| serde_json::to_string(p).unwrap_or_default())
                .unwrap_or_default();
            (loose_id, loose_name, content)
        }
    };
    let tool_use_id = tool_use_id_opt.unwrap_or_else(|| {
        let id = format!("tool_{}", *fallback_id);
        *fallback_id += 1;
        id
    });
    let tool_name = tool_name_opt.unwrap_or_default();
    let content = emphasize_tool_error_for_upstream(payload, content);
    (tool_use_id, tool_name, content)
}

/// Same fallback counter progression as [`session_messages_to_upstream_entries`] (tool results and
/// assistant `requested_tool_calls` with missing ids), used to align tool-call ids when cleaning.
fn prefix_fallback_before_each_message(
    messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
) -> Vec<usize> {
    let len = messages.len();
    let mut prefix = vec![0usize; len];
    let mut fallback = 0usize;
    for i in 0..len {
        prefix[i] = fallback;
        let &(role, type_, ref payload) = &messages[i];
        match (role, type_) {
            (ChatSessionRoleType::Ai, msg_type) if is_tool_result_type(&msg_type) => {
                let _ = tool_result_from_payload(&msg_type, payload, &mut fallback, false);
            }
            (ChatSessionRoleType::Ai, ChatMessageType::Text) => {
                if let Some(tp) = parse_text_payload(payload)
                    && let Some(ref calls) = tp.requested_tool_calls
                {
                    for tc in calls {
                        let _ = tc.tool_call_id.clone().unwrap_or_else(|| {
                            let id = format!("tool_{}", fallback);
                            fallback += 1;
                            id
                        });
                    }
                }
            }
            _ => {}
        }
    }
    prefix
}

fn resolved_tool_call_ids_for_text_payload(
    tp: &TextPayload,
    fb_before: usize,
) -> (Vec<String>, usize) {
    let mut fb = fb_before;
    let calls = tp.requested_tool_calls.as_deref().unwrap_or(&[]);
    let mut ids = Vec::with_capacity(calls.len());
    for tc in calls {
        let id = tc.tool_call_id.clone().unwrap_or_else(|| {
            let id = format!("tool_{}", fb);
            fb += 1;
            id
        });
        ids.push(id);
    }
    (ids, fb)
}

fn is_ai_tool_result_at(
    messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    k: usize,
) -> bool {
    messages[k].0 == ChatSessionRoleType::Ai && is_tool_result_type(&messages[k].1)
}

/// Index of the message immediately before a consecutive run of AI tool-result rows that includes
/// `tool_result_index`. That message should be the assistant `Text` with `requested_tool_calls` for
/// the first tool in the batch (when history is consistent).
fn assistant_index_before_tool_block(
    messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    tool_result_index: usize,
) -> Option<usize> {
    if tool_result_index == 0 {
        return None;
    }
    let mut k = tool_result_index - 1;
    loop {
        if !is_ai_tool_result_at(messages, k) {
            break;
        }
        if k == 0 {
            return None;
        }
        k -= 1;
    }
    Some(k)
}

/// Removes tool-result session messages that have no matching assistant `tool_use` in the loaded history.
///
/// This happens when loaders only include **completed** rows: e.g. the planner assistant turn that
/// issued `implement_solution` is `stopped`/`failed` and omitted, but the async tool row completed
/// and remains — Claude then sees a `tool_result` without a preceding `tool_use`.
pub fn remove_tool_result_messages_without_matching_assistant_tool_use(
    messages: &mut Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>,
) {
    let len = messages.len();
    if len == 0 {
        return;
    }

    let mut to_remove: Vec<usize> = Vec::new();
    for i in 0..len {
        if !is_ai_tool_result_at(messages, i) {
            continue;
        }
        let prefix = prefix_fallback_before_each_message(messages);
        let mut fb = prefix[i];
        let (tool_id, tool_name, _) =
            tool_result_from_payload(&messages[i].1, &messages[i].2, &mut fb, false);
        if tool_name.is_empty() {
            to_remove.push(i);
            continue;
        }
        let Some(a_idx) = assistant_index_before_tool_block(messages, i) else {
            to_remove.push(i);
            continue;
        };
        if messages[a_idx].0 != ChatSessionRoleType::Ai
            || messages[a_idx].1 != ChatMessageType::Text
        {
            to_remove.push(i);
            continue;
        }
        let payload_opt = messages[a_idx].2.clone();
        let Some(tp) = parse_text_payload(&payload_opt) else {
            to_remove.push(i);
            continue;
        };
        let (resolved_ids, _) = resolved_tool_call_ids_for_text_payload(&tp, prefix[a_idx]);
        if !resolved_ids.contains(&tool_id) {
            to_remove.push(i);
        }
    }

    to_remove.sort_unstable();
    to_remove.dedup();
    for idx in to_remove.into_iter().rev() {
        messages.remove(idx);
    }
}

/// Removes assistant `requested_tool_calls` that never received a matching tool-result message
/// (e.g. user stopped generation). Also drops calls that are only partially fulfilled when a
/// non–tool-result message appears after the consecutive tool-result block.
///
/// Matches [`session_messages_to_upstream_entries`] id assignment via [`prefix_fallback_before_each_message`].
pub fn trim_assistant_requested_tool_calls_to_match_following_tool_results(
    messages: &mut Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>,
) {
    let len = messages.len();
    if len == 0 {
        return;
    }

    for i in 0..len {
        let prefix = prefix_fallback_before_each_message(messages);
        let (role, type_, payload_opt) = {
            let m = &mut messages[i];
            (m.0, m.1, m.2.clone())
        };
        if role != ChatSessionRoleType::Ai || type_ != ChatMessageType::Text {
            continue;
        }
        let Some(mut tp) = parse_text_payload(&payload_opt) else {
            continue;
        };
        let Some(ref calls) = tp.requested_tool_calls else {
            continue;
        };
        if calls.is_empty() {
            continue;
        }

        let fb_before = prefix[i];
        let (resolved_ids, _) = resolved_tool_call_ids_for_text_payload(&tp, fb_before);

        let next_is_tool_result = i + 1 < len && {
            let n = &mut messages[i + 1];
            (n.0 == ChatSessionRoleType::Ai) && is_tool_result_type(&n.1)
        };

        if !next_is_tool_result {
            tp.requested_tool_calls = None;
            if let Ok(v) = serde_json::to_value(&tp) {
                messages[i].2 = Some(v);
            }
            continue;
        }

        let mut j = i + 1;
        let mut fb2 = prefix[i + 1];
        let mut fulfilled = HashSet::new();
        while j < len {
            let (r, t, payload_ref) = {
                let m = &mut messages[j];
                (m.0, m.1, m.2.clone())
            };
            if r != ChatSessionRoleType::Ai || !is_tool_result_type(&t) {
                break;
            }
            let (tool_use_id, tool_name, _) =
                tool_result_from_payload(&t, &payload_ref, &mut fb2, false);
            if !tool_name.is_empty() {
                fulfilled.insert(tool_use_id);
            }
            j += 1;
        }

        let filtered: Vec<_> = calls
            .iter()
            .zip(resolved_ids.iter())
            .filter(|(_, rid)| fulfilled.contains(*rid))
            .map(|(tc, _)| tc.clone())
            .collect();

        tp.requested_tool_calls = if filtered.is_empty() {
            None
        } else {
            Some(filtered)
        };
        if let Ok(v) = serde_json::to_value(&tp) {
            messages[i].2 = Some(v);
        }
    }
}

fn reject_controller_box_upstream_notice(payload: &Option<Value>) -> Option<String> {
    let p = ChatMessagePayload::from_value(&ChatMessageType::RejectControllerBox, payload.clone())
        .ok()
        .flatten()?;
    let ChatMessagePayload::RejectControllerBox(box_payload) = p else {
        return None;
    };
    let ids: Vec<String> = box_payload
        .unit_changes
        .iter()
        .map(|u| u.id.clone())
        .collect();

    if box_payload.is_rejected {
        return Some(format!(
            "The user rejected the changes from your last assistant turn. Removed units: {}.",
            ids.join(", ")
        ));
    }

    Some(format!(
        "The user has accepted the changes from your last assistant turn for these units: {}. \
This is not a stage or production release and implies nothing about shipping—it only means the user did not reject those edits, so on your next turn you may treat these units as the ones to rely on and build something upon them.",
        ids.join(", ")
    ))
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
        if is_excluded_from_upstream_llm_history(type_) {
            continue;
        }
        match (role, type_) {
            (ChatSessionRoleType::User, _) => {
                let text = expand_baal_inlines_for_llm_user_text(&payload_to_string(payload));
                let text = text.trim().to_string();
                if !text.is_empty() {
                    out.push(UpstreamHistoryEntry::User { text });
                }
            }
            (ChatSessionRoleType::Ai, msg_type) if is_tool_result_type(msg_type) => {
                let (tool_use_id, tool_name, content) =
                    tool_result_from_payload(msg_type, payload, &mut fallback_tool_id, true);
                if tool_name.is_empty() {
                    tracing::debug!("skipping tool result message: empty tool_name in payload");
                } else {
                    out.push(UpstreamHistoryEntry::ToolResult {
                        tool_call_id: tool_use_id,
                        tool_name,
                        content,
                    });
                }
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
                                    let id = tc.tool_call_id.clone().unwrap_or_else(|| {
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
                    let thinking_blocks = tp.thinking_blocks.clone().filter(|b| !b.is_empty());
                    if !text.is_empty() || tool_calls.is_some() || thinking_blocks.is_some() {
                        out.push(UpstreamHistoryEntry::Assistant {
                            text,
                            tool_calls,
                            thinking_blocks,
                        });
                    }
                }
            }
            (ChatSessionRoleType::System, ChatMessageType::RejectControllerBox) => {
                if let Some(text) = reject_controller_box_upstream_notice(payload)
                    && !text.is_empty() {
                        out.push(UpstreamHistoryEntry::System { text });
                    }
            }
            (ChatSessionRoleType::System, _) => {}
            _ => {
                let text = payload_to_string(payload).trim().to_string();
                if text.is_empty() {
                    continue;
                }
                match role {
                    ChatSessionRoleType::User => {
                        out.push(UpstreamHistoryEntry::User { text });
                    }
                    ChatSessionRoleType::Ai => {
                        out.push(UpstreamHistoryEntry::Assistant {
                            text,
                            tool_calls: None,
                            thinking_blocks: None,
                        });
                    }
                    ChatSessionRoleType::System => {}
                }
            }
        }
    }

    let user_for_llm = expand_baal_inlines_for_llm_user_text(user_text);
    if !user_for_llm.trim().is_empty() {
        out.push(UpstreamHistoryEntry::User {
            text: user_for_llm.trim().to_string(),
        });
    }

    out
}
