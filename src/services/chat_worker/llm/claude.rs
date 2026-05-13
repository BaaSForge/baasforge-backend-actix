// Claude (Anthropic): streaming and tool use via pure HTTP (reqwest + serde_json).
// Extended thinking: https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageType, ChatSessionRoleType, RequestedToolCall, TextPayload,
    ThinkingBlock,
};
use crate::routes::ws::events::{
    ChatSessionNewMessageChunkPayload, ChatSessionNewThinkingChunkPayload, WsServerEvent,
};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::services::chat_worker::agent::system_prompt_for_agent;
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::llm::GenerateOutcome;
use crate::services::chat_worker::llm::history::{
    UpstreamHistoryEntry, session_messages_to_upstream_entries,
};
use futures::StreamExt;
use redis::aio::MultiplexedConnection;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::BoxError;
use super::tools;
use serde::Serialize;
use serde_json::Value;

// ---- Request/response types (no crate) ----

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Anthropic requires `max_tokens` > `thinking.budget_tokens`. Extra budget for visible reply / tool payloads.
const MAX_TOKENS_HEADROOM_ABOVE_THINKING: u32 = 8192;

fn resolve_max_tokens(thinking_budget_tokens: Option<u32>) -> u32 {
    match thinking_budget_tokens {
        None => DEFAULT_MAX_TOKENS,
        Some(budget) => budget.saturating_add(MAX_TOKENS_HEADROOM_ABOVE_THINKING),
    }
}
/// Beta header for interleaved extended thinking + tool use (Claude 4 family).
const BETA_INTERLEAVED_THINKING: &str = "interleaved-thinking-2025-05-14";

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ClaudeContentBlock {
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct ClaudeMessage {
    role: String,
    content: Vec<ClaudeContentBlock>,
}

#[derive(Debug)]
enum BlockKind {
    Thinking,
    Text,
    /// Tool id, name, and initial `input` from `content_block_start` (may be replaced by streamed JSON).
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

/// Convert upstream history entries to Claude API messages.
/// Tool-call request and tool results are always separate messages (never combined):
fn entries_to_claude_messages(entries: &[UpstreamHistoryEntry]) -> Vec<ClaudeMessage> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            UpstreamHistoryEntry::User { text } => {
                if !text.is_empty() {
                    out.push(ClaudeMessage {
                        role: "user".into(),
                        content: vec![ClaudeContentBlock::Text { text: text.clone() }],
                    });
                }
            }
            UpstreamHistoryEntry::Assistant {
                text,
                tool_calls,
                thinking_blocks,
            } => {
                let mut blocks: Vec<ClaudeContentBlock> = Vec::new();
                // Anthropic: replay each stored thinking block with both `thinking` and `signature`
                // (see extended-thinking “Preserving thinking blocks”). Order: thinking → text → tool_use.
                if let Some(tbs) = thinking_blocks {
                    for tb in tbs {
                        let signature = tb.signature.clone().unwrap_or_default();
                        if !tb.thinking.is_empty() && signature.is_empty() {
                            tracing::warn!(
                                "Claude history: thinking block has visible text but empty signature; \
                                 API may reject or degrade continuity — check stream capture"
                            );
                        }
                        blocks.push(ClaudeContentBlock::Thinking {
                            thinking: tb.thinking.clone(),
                            signature,
                        });
                    }
                }
                if !text.trim().is_empty() {
                    blocks.push(ClaudeContentBlock::Text {
                        text: text.trim().to_string(),
                    });
                }
                if let Some(tcs) = tool_calls.as_ref() {
                    for tc in tcs {
                        blocks.push(ClaudeContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input: tc.arguments.clone(),
                        });
                    }
                }
                if !blocks.is_empty() {
                    out.push(ClaudeMessage {
                        role: "assistant".into(),
                        content: blocks,
                    });
                }
            }
            UpstreamHistoryEntry::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                out.push(ClaudeMessage {
                    role: "user".into(),
                    content: vec![ClaudeContentBlock::ToolResult {
                        tool_use_id: tool_call_id.clone(),
                        content: content.clone(),
                    }],
                });
            }
            // Anthropic: `system` is a single top-level field on the request, not a row in
            // `messages` (only user/assistant there). Interleaved notices use a user message.
            UpstreamHistoryEntry::System { text } => {
                if !text.is_empty() {
                    out.push(ClaudeMessage {
                        role: "user".into(),
                        content: vec![ClaudeContentBlock::Text { text: text.clone() }],
                    });
                }
            }
        }
    }
    out
}

/// Build request body for POST /v1/messages (streaming, with tools).
fn build_request_body(
    system_prompt: &str,
    messages: &[ClaudeMessage],
    tools_json: Vec<Value>,
    model: &str,
    thinking_budget_tokens: Option<u32>,
) -> Value {
    let messages_value = serde_json::to_value(messages).expect("ClaudeMessage serialization");
    let max_tokens = resolve_max_tokens(thinking_budget_tokens);
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
        "system": system_prompt,
        "messages": messages_value,
        "tools": tools_json,
    });
    if let Some(budget) = thinking_budget_tokens
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert(
            "thinking".to_string(),
            serde_json::json!({
                "type": "enabled",
                "budget_tokens": budget,
            }),
        );
    }
    body
}

/// Build tools array for Claude Messages API. Each tool: name, description, input_schema (JSON Schema).
/// Matches Anthropic format: https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/implement-tool-use
fn to_tools_json(agent: &ChatAgentType) -> Vec<Value> {
    let names = tools::tool_names_for_llm_session(agent);
    tools::all_tool_specs()
        .into_iter()
        .filter(|t| names.contains(&t.name))
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })
        })
        .collect()
}

// ---- SSE parsing ----

enum BlockStartKind {
    Thinking,
    Text,
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

enum StreamEvent {
    BlockStart { index: usize, kind: BlockStartKind },
    ThinkingDelta { index: usize, thinking: String },
    TextDelta { text: String },
    SignatureDelta { index: usize, signature: String },
    InputJsonDelta { index: usize, partial_json: String },
    MessageStop,
}

async fn read_one_sse_event<S, E>(
    stream: &mut S,
    buf: &mut Vec<u8>,
) -> Result<Option<StreamEvent>, BoxError>
where
    S: futures::Stream<Item = Result<Vec<u8>, E>> + Unpin,
    E: std::fmt::Display,
{
    loop {
        if let Some(newline) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line).trim().to_string();
            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let v: Value =
                    serde_json::from_str(data).map_err(|e| format!("Claude SSE JSON: {}", e))?;
                let event_type = v.get("type").and_then(|t| t.as_str());

                if event_type
                    .map(|t| t.eq_ignore_ascii_case("ping"))
                    .unwrap_or(false)
                {
                    continue;
                }

                if event_type == Some("content_block_delta") {
                    let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    if let Some(delta) = v.get("delta") {
                        let delta_type = delta.get("type").and_then(|t| t.as_str());
                        if delta_type == Some("input_json_delta") {
                            let partial_json = delta
                                .get("partial_json")
                                .and_then(|p| p.as_str())
                                .unwrap_or("")
                                .to_string();
                            return Ok(Some(StreamEvent::InputJsonDelta {
                                index,
                                partial_json,
                            }));
                        }
                        if delta_type == Some("thinking_delta") {
                            let thinking = delta
                                .get("thinking")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            return Ok(Some(StreamEvent::ThinkingDelta { index, thinking }));
                        }
                        if delta_type == Some("signature_delta") {
                            let signature = delta
                                .get("signature")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            return Ok(Some(StreamEvent::SignatureDelta { index, signature }));
                        }
                        if delta_type == Some("text_delta") {
                            let text = delta
                                .get("text")
                                .and_then(|t| t.as_str())
                                .unwrap_or("")
                                .to_string();
                            return Ok(Some(StreamEvent::TextDelta { text }));
                        }
                    }
                    continue;
                }

                if event_type == Some("content_block_start") {
                    let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    if let Some(block) = v.get("content_block") {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("thinking") => {
                                return Ok(Some(StreamEvent::BlockStart {
                                    index,
                                    kind: BlockStartKind::Thinking,
                                }));
                            }
                            Some("text") => {
                                return Ok(Some(StreamEvent::BlockStart {
                                    index,
                                    kind: BlockStartKind::Text,
                                }));
                            }
                            Some("tool_use") => {
                                let id = block
                                    .get("id")
                                    .and_then(|i| i.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let input = block.get("input").cloned().unwrap_or(Value::Null);
                                return Ok(Some(StreamEvent::BlockStart {
                                    index,
                                    kind: BlockStartKind::ToolUse { id, name, input },
                                }));
                            }
                            _ => {}
                        }
                    }
                    continue;
                }

                if event_type == Some("message_stop") {
                    return Ok(Some(StreamEvent::MessageStop));
                }
            }
            continue;
        }
        let chunk = match stream.next().await {
            Some(Ok(c)) => c,
            Some(Err(e)) => return Err(format!("Claude stream: {}", e).into()),
            None => return Ok(None),
        };
        buf.extend_from_slice(&chunk);
    }
}

fn build_thinking_blocks_from_stream_state(
    block_kinds: &HashMap<usize, BlockKind>,
    thinking_buffers: &HashMap<usize, String>,
    signature_buffers: &HashMap<usize, String>,
) -> Option<Vec<ThinkingBlock>> {
    let mut indices: Vec<usize> = block_kinds
        .iter()
        .filter(|(_, k)| matches!(k, BlockKind::Thinking))
        .map(|(i, _)| *i)
        .collect();
    indices.sort_unstable();
    if indices.is_empty() {
        return None;
    }
    let mut blocks = Vec::new();
    for i in indices {
        let thinking = thinking_buffers.get(&i).cloned().unwrap_or_default();
        let sig = signature_buffers.get(&i).cloned().unwrap_or_default();
        if !thinking.is_empty() || !sig.is_empty() {
            blocks.push(ThinkingBlock {
                thinking: thinking.clone(),
                signature: if sig.is_empty() { None } else { Some(sig) },
            });
        }
    }
    if blocks.is_empty() {
        None
    } else {
        Some(blocks)
    }
}

pub async fn call_claude_stream(
    api_key: &str,
    model: &str,
    thinking_budget_tokens: Option<u32>,
    pool: &crate::database::DbPool,
    session_messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    user_text: &str,
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
    message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    agent: ChatAgentType,
    queue_invoking_message_id: Option<Uuid>,
    stale_base_release: bool,
    base_runtime_endpoint: &str,
) -> Result<GenerateOutcome, BoxError> {
    let system_prompt = system_prompt_for_agent(
        &agent,
        stale_base_release,
        project_id,
        base_runtime_endpoint,
    );
    let tools_list = to_tools_json(&agent);
    let tool_count = tools_list.len();

    let entries = session_messages_to_upstream_entries(session_messages, user_text);
    let messages = entries_to_claude_messages(&entries);
    if messages.is_empty() {
        return Err(
            "Claude: no messages to send (session history and user text are empty or whitespace)"
                .into(),
        );
    }

    let body = build_request_body(
        system_prompt.as_ref(),
        &messages,
        tools_list.clone(),
        model,
        thinking_budget_tokens,
    );

    let url = format!("{}/v1/messages", ANTHROPIC_API_BASE);
    let mut req = reqwest::Client::new()
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json");

    if !tools_list.is_empty() {
        req = req.header("anthropic-beta", BETA_INTERLEAVED_THINKING);
    }
    let response = req
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Claude request: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let err_body = response.text().await.unwrap_or_default();
        return Err(format!("Claude stream: {}: {}", status, err_body).into());
    }

    let mut stream = response
        .bytes_stream()
        .map(|r| r.map(|b| b.to_vec()).map_err(|e| e.to_string()));
    let mut buf: Vec<u8> = Vec::new();
    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;
    let mut thinking_chunk_index: u32 = 0;
    let mut accumulated_tool_input: HashMap<usize, String> = HashMap::new();
    let mut block_kinds: HashMap<usize, BlockKind> = HashMap::new();
    let mut thinking_buffers: HashMap<usize, String> = HashMap::new();
    let mut signature_buffers: HashMap<usize, String> = HashMap::new();

    tracing::info!(
        tool_count,
        model,
        ?thinking_budget_tokens,
        "Claude: stream started with tools"
    );

    loop {
        let event = match read_one_sse_event(&mut stream, &mut buf).await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => return Err(e),
        };
        match event {
            StreamEvent::BlockStart { index, kind } => match kind {
                BlockStartKind::Thinking => {
                    block_kinds.insert(index, BlockKind::Thinking);
                }
                BlockStartKind::Text => {
                    block_kinds.insert(index, BlockKind::Text);
                }
                BlockStartKind::ToolUse { id, name, input } => {
                    block_kinds.insert(index, BlockKind::ToolUse { id, name, input });
                }
            },
            StreamEvent::ThinkingDelta { index, thinking } => {
                if !thinking.is_empty() {
                    thinking_buffers
                        .entry(index)
                        .or_default()
                        .push_str(&thinking);
                    if let Some(ref conn) = redis_conn {
                        let mut guard = conn.lock().await;
                        if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                            return Ok(GenerateOutcome::Stopped(TextPayload {
                                text: full_text,
                                requested_tool_calls: None,
                                thinking_blocks: build_thinking_blocks_from_stream_state(
                                    &block_kinds,
                                    &thinking_buffers,
                                    &signature_buffers,
                                ),
                            }));
                        }
                        let payload = ChatSessionNewThinkingChunkPayload::new(
                            project_id,
                            session_id,
                            message_id,
                            thinking_chunk_index,
                            thinking,
                            false,
                        );
                        let ws_event = WsServerEvent::ChatSessionNewThinkingChunk(payload);
                        if let Err(e) =
                            publish_event_to_channel(&mut *guard, topic, &ws_event).await
                        {
                            tracing::warn!("Failed to publish thinking chunk to Redis: {}", e);
                        }
                        thinking_chunk_index += 1;
                    }
                }
            }
            StreamEvent::TextDelta { text } => {
                if !text.is_empty() {
                    full_text.push_str(&text);
                    if let Some(ref conn) = redis_conn {
                        let mut guard = conn.lock().await;
                        if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                            return Ok(GenerateOutcome::Stopped(TextPayload {
                                text: full_text,
                                requested_tool_calls: None,
                                thinking_blocks: build_thinking_blocks_from_stream_state(
                                    &block_kinds,
                                    &thinking_buffers,
                                    &signature_buffers,
                                ),
                            }));
                        }
                        let payload = ChatSessionNewMessageChunkPayload::new(
                            project_id,
                            session_id,
                            message_id,
                            chunk_index,
                            text,
                            false,
                        );
                        let ws_event = WsServerEvent::ChatSessionNewMessageChunk(payload);
                        if let Err(e) =
                            publish_event_to_channel(&mut *guard, topic, &ws_event).await
                        {
                            tracing::warn!("Failed to publish chat chunk to Redis: {}", e);
                        }
                        chunk_index += 1;
                    }
                }
            }
            StreamEvent::SignatureDelta { index, signature } => {
                if !signature.is_empty() {
                    signature_buffers
                        .entry(index)
                        .or_default()
                        .push_str(&signature);
                }
            }
            StreamEvent::InputJsonDelta {
                index,
                partial_json,
            } => {
                accumulated_tool_input
                    .entry(index)
                    .or_default()
                    .push_str(&partial_json);
            }
            StreamEvent::MessageStop => break,
        }
    }

    let mut tool_entries: Vec<(usize, String, String, Value)> = block_kinds
        .iter()
        .filter_map(|(idx, kind)| {
            if let BlockKind::ToolUse { id, name, input } = kind {
                Some((*idx, id.clone(), name.clone(), input.clone()))
            } else {
                None
            }
        })
        .collect();
    tool_entries.sort_by_key(|(idx, _, _, _)| *idx);
    let tool_uses: Vec<(String, String, Value)> = tool_entries
        .into_iter()
        .map(|(index, id, name, initial_input)| {
            let input = accumulated_tool_input
                .remove(&index)
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(initial_input);
            (id, name, input)
        })
        .collect();

    let thinking_blocks = build_thinking_blocks_from_stream_state(
        &block_kinds,
        &thinking_buffers,
        &signature_buffers,
    );

    // True if we received visible text/thinking deltas (vs tool-only / signature-only stream).
    let had_content_stream = !full_text.is_empty()
        || thinking_blocks
            .as_ref()
            .is_some_and(|b| b.iter().any(|blk| !blk.thinking.is_empty()));
    let published_stream_chunks = chunk_index > 0 || thinking_chunk_index > 0;

    tracing::info!(
        tool_use_count = tool_uses.len(),
        "Claude: stream ended, checking for tool calls"
    );

    let requested_tool_calls: Option<Vec<RequestedToolCall>> = if tool_uses.is_empty() {
        tracing::info!("Claude: no tool calls in stream");
        None
    } else {
        if let Some(ref conn) = redis_conn {
            let mut guard = conn.lock().await;
            if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                return Ok(GenerateOutcome::Stopped(TextPayload {
                    text: full_text,
                    requested_tool_calls: None,
                    thinking_blocks: thinking_blocks.clone(),
                }));
            }
        }
        let requested: Vec<RequestedToolCall> = tool_uses
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, input)| RequestedToolCall {
                tool_call_id: if id.is_empty() { None } else { Some(id) },
                tool_name: name.clone(),
                tool_arguments_to_pass: input,
                thought_signature: None,
            })
            .collect();
        if requested.is_empty() {
            None
        } else {
            tracing::info!(
                "Claude: LLM requested {} tool call(s), executing",
                requested.len()
            );
            if !had_content_stream {
                let payload = TextPayload {
                    text: full_text.clone(),
                    requested_tool_calls: Some(requested.clone()),
                    thinking_blocks: thinking_blocks.clone(),
                };
                if let Some(ref conn) = redis_conn {
                    ai_message::create_ai_message_with_payload_and_publish(
                        pool,
                        conn,
                        session_id,
                        message_id,
                        user_id,
                        project_id,
                        &payload,
                        agent.clone(),
                        queue_invoking_message_id,
                    )
                    .await
                    .map_err(|e| format!("ai_message: {}", e))?;
                }
            }
            if let Some(ref conn) = redis_conn {
                let mut guard = conn.lock().await;
                if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                    return Ok(GenerateOutcome::Stopped(TextPayload {
                        text: full_text,
                        requested_tool_calls: None,
                        thinking_blocks: thinking_blocks.clone(),
                    }));
                }
            }
            for r in &requested {
                tracing::info!(tool = %r.tool_name, args = ?r.tool_arguments_to_pass, "Claude: executing tool");
                if let Err(e) = tools::execute_tool_async(
                    &r.tool_name,
                    r.tool_arguments_to_pass.clone(),
                    r.tool_call_id.clone(),
                )
                .await
                {
                    tracing::warn!(
                        tool = %r.tool_name,
                        "Tool arg validation failed (Failed row persisted); continuing remaining tool calls: {}",
                        e
                    );
                }
            }
            Some(requested)
        }
    };

    if published_stream_chunks && let Some(ref conn) = redis_conn {
        let mut guard = conn.lock().await;
        if thinking_chunk_index > 0 {
            let thinking_done = ChatSessionNewThinkingChunkPayload::new(
                project_id,
                session_id,
                message_id,
                thinking_chunk_index,
                String::new(),
                true,
            );
            if let Err(e) = publish_event_to_channel(
                &mut *guard,
                topic,
                &WsServerEvent::ChatSessionNewThinkingChunk(thinking_done),
            )
            .await
            {
                tracing::warn!("Failed to publish final thinking chunk to Redis: {}", e);
            }
        }
        let payload = ChatSessionNewMessageChunkPayload::new(
            project_id,
            session_id,
            message_id,
            chunk_index,
            String::new(),
            true,
        );
        let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
        if let Err(e) = publish_event_to_channel(&mut *guard, topic, &event).await {
            tracing::warn!("Failed to publish final chat chunk to Redis: {}", e);
        }
    }

    Ok(GenerateOutcome::Completed(TextPayload {
        text: full_text,
        requested_tool_calls,
        thinking_blocks,
    }))
}
