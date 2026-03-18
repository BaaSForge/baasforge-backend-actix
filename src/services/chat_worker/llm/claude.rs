// Claude (Anthropic): streaming and tool use.
// Uses official API model IDs (e.g. claude-sonnet-4-20250514); anthropic_rust types for request/response.
use crate::models::chat_messages::{ChatAgentType, RequestedToolCall, ChatSessionRoleType, TextPayload};
use crate::services::chat_worker::agent::system_prompt_for_agent;
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::llm::GenerateOutcome;
use anthropic_rust::{Client, ContentBlock, Role, StreamEvent, Tool};
use futures::StreamExt;
use redis::aio::MultiplexedConnection;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::tools;
use super::{payload_to_string, BoxError};
use crate::services::chat_worker::agent::tool_names_for_agent;
use serde_json::Value;

/// Wrapper for SSE events: crate events plus API-only variants (ping skipped; input_json_delta handled here).
enum ClaudeStreamEvent {
    Event(StreamEvent),
    InputJsonDelta { index: usize, partial_json: String },
}

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Official API model IDs (see https://docs.anthropic.com/en/docs/about-claude/models/overview).
/// The anthropic_rust crate uses different IDs that are retired; we send the correct ones.
fn api_model_id(config_model: &str) -> &'static str {
    match config_model {
        "claude-sonnet-4-6" => "claude-sonnet-4-6",
        "claude-sonnet-4-5" | "claude-sonnet-4-5-20250929" => "claude-sonnet-4-5-20250929",
        "claude-4-sonnet-20250514" | "claude-sonnet-4-20250514" => "claude-sonnet-4-20250514",
        _ => "claude-sonnet-4-20250514",
    }
}

/// Read one SSE "data: {...}" event from the stream; normalize message_start.model so crate can deserialize.
/// Returns crate StreamEvent or InputJsonDelta (API sends input_json_delta for streaming tool input; anthropic_rust only has text_delta).
async fn read_one_sse_event<S, E>(
    stream: &mut S,
    buf: &mut Vec<u8>,
) -> Result<Option<ClaudeStreamEvent>, BoxError>
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
                let mut v: Value =
                    serde_json::from_str(data).map_err(|e| format!("Claude SSE JSON: {}", e))?;
                let event_type = v.get("type").and_then(|t| t.as_str());
                // Skip keep-alive ping events (API may send "ping" or "Ping"); anthropic_rust::StreamEvent does not include this variant.
                if event_type.map(|t| t.eq_ignore_ascii_case("ping")).unwrap_or(false) {
                    continue;
                }
                // content_block_delta with delta.type == "input_json_delta" is not in anthropic_rust::ContentDelta; handle it here.
                if event_type == Some("content_block_delta") {
                    if let Some(delta_type) = v.get("delta").and_then(|d| d.get("type")).and_then(|t| t.as_str()) {
                        if delta_type == "input_json_delta" {
                            let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            let partial_json = v
                                .get("delta")
                                .and_then(|d| d.get("partial_json"))
                                .and_then(|p| p.as_str())
                                .unwrap_or("")
                                .to_string();
                            return Ok(Some(ClaudeStreamEvent::InputJsonDelta { index, partial_json }));
                        }
                    }
                }
                if event_type == Some("message_start") {
                    if let Some(msg) = v.get_mut("message") {
                        msg["model"] = Value::String("claude-4-sonnet-20250514".to_string());
                    }
                }
                let event: StreamEvent =
                    serde_json::from_value(v).map_err(|e| format!("Claude SSE event: {}", e))?;
                return Ok(Some(ClaudeStreamEvent::Event(event)));
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

fn to_claude_tools_for_agent(agent: &ChatAgentType) -> Vec<Tool> {
    let names = tool_names_for_agent(agent);
    tools::all_tool_specs()
        .into_iter()
        .filter(|t| names.contains(&t.name))
        .map(|t| {
            Tool::builder(t.name)
                .description(t.description)
                .schema_value(t.parameters.clone())
                .build()
        })
        .collect()
}

fn build_messages(
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
) -> Vec<(Role, ContentBlock)> {
    let mut v: Vec<(Role, ContentBlock)> = Vec::new();
    for (role, payload) in session_messages {
        let c = payload_to_string(payload).trim().to_string();
        if c.is_empty() {
            continue;
        }
        let role_enum = match role {
            ChatSessionRoleType::User => Role::User,
            ChatSessionRoleType::Ai => Role::Assistant,
        };
        v.push((role_enum, ContentBlock::text(c)));
    }
    // Claude API requires text content blocks to be non-empty; skip empty or whitespace-only user message.
    let user_text = user_text.trim();
    if !user_text.is_empty() {
        v.push((Role::User, ContentBlock::text(user_text.to_string())));
    }
    v
}

pub async fn call_claude_stream(
    api_key: &str,
    model: &str,
    pool: &crate::database::DbPool,
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
    message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    agent: ChatAgentType,
) -> Result<GenerateOutcome, BoxError> {
    let system_prompt = system_prompt_for_agent(&agent);
    let tools_list = to_claude_tools_for_agent(&agent);
    let tool_count = tools_list.len();
    let api_model = api_model_id(model);

    let client = Client::builder()
        .api_key(api_key)
        .model(anthropic_rust::Model::Claude4Sonnet20250514)
        .max_tokens(8192)
        .build()
        .map_err(|e| format!("Claude client: {}", e))?;

    let messages = build_messages(session_messages, user_text);
    if messages.is_empty() {
        return Err("Claude: no messages to send (session history and user text are empty or whitespace)".into());
    }
    let mut builder = client.chat_builder().system(system_prompt);
    for (role, content) in messages {
        builder = builder.message(role, content);
    }
    let request = builder.tools(tools_list).build();

    let mut body = serde_json::to_value(&request).map_err(|e| format!("Claude request serialize: {}", e))?;
    body["model"] = Value::String(api_model.to_string());
    body["max_tokens"] = Value::Number(8192u32.into());
    body["stream"] = Value::Bool(true);

    let url = format!("{}/v1/messages", ANTHROPIC_API_BASE);
    let response = reqwest::Client::new()
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Claude request: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let err_body = response.text().await.unwrap_or_default();
        return Err(format!("Claude stream: {}: {}", status, err_body).into());
    }

    let mut stream = response.bytes_stream().map(|r| {
        r.map(|b| b.to_vec()).map_err(|e| e.to_string())
    });
    let mut buf: Vec<u8> = Vec::new();
    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;
    let mut message_created = false;
    // Tool use: index -> (id, name, initial input from block); input may be streamed via input_json_delta.
    let mut tool_use_by_index: HashMap<usize, (String, String, Value)> = HashMap::new();
    let mut accumulated_tool_input: HashMap<usize, String> = HashMap::new();

    tracing::info!(tool_count, api_model, "Claude: stream started with tools");

    loop {
        let event = match read_one_sse_event(&mut stream, &mut buf).await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => return Err(e),
        };
        match &event {
            ClaudeStreamEvent::Event(StreamEvent::ContentBlockDelta { delta, .. }) => {
                if let anthropic_rust::ContentDelta::TextDelta { text } = delta {
                    if !text.is_empty() {
                        if let Some(ref conn) = redis_conn {
                            ai_message::ensure_ai_message_created_and_publish(
                                pool,
                                conn,
                                session_id,
                                message_id,
                                user_id,
                                project_id,
                                agent.clone(),
                            )
                            .await
                            .map_err(|e| format!("ensure_ai_message_created_and_publish: {}", e))?;
                        }
                        message_created = true;
                        full_text.push_str(text);
                        if let Some(ref conn) = redis_conn {
                            let mut guard = conn.lock().await;
                            if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                                return Ok(GenerateOutcome::Stopped(TextPayload {
                                    text: full_text,
                                    requested_tool_calls: None,
                                }));
                            }
                            let payload = ChatSessionNewMessageChunkPayload::new(
                                project_id,
                                session_id,
                                message_id,
                                chunk_index,
                                text.to_string(),
                                false,
                            );
                            let ws_event = WsServerEvent::ChatSessionNewMessageChunk(payload);
                            if let Err(e) = publish_event_to_channel(&mut *guard, topic, &ws_event).await {
                                tracing::warn!("Failed to publish chat chunk to Redis: {}", e);
                            }
                            chunk_index += 1;
                        }
                    }
                }
            }
            ClaudeStreamEvent::Event(StreamEvent::ContentBlockStart { index, content_block }) => {
                if let ContentBlock::ToolUse { id, name, input } = content_block {
                    tool_use_by_index.insert(*index, (id.clone(), name.clone(), input.clone()));
                }
            }
            ClaudeStreamEvent::InputJsonDelta { index, partial_json } => {
                accumulated_tool_input
                    .entry(*index)
                    .or_default()
                    .push_str(partial_json);
            }
            ClaudeStreamEvent::Event(StreamEvent::MessageStop) => break,
            _ => {}
        }
    }

    // Build tool_uses: use accumulated input from input_json_delta if present, else initial from ContentBlockStart.
    let tool_uses: Vec<(String, String, Value)> = tool_use_by_index
        .into_iter()
        .map(|(index, (id, name, initial_input))| {
            let input = accumulated_tool_input
                .remove(&index)
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(initial_input);
            (id, name, input)
        })
        .collect();

    tracing::info!(
        tool_use_count = tool_uses.len(),
        "Claude: stream ended, checking for tool calls"
    );

    let requested_tool_calls: Option<Vec<RequestedToolCall>> = if tool_uses.is_empty() {
        tracing::info!("Claude: no tool calls in stream");
        None
    } else {
        let requested: Vec<RequestedToolCall> = tool_uses
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, input)| RequestedToolCall {
                tool_call_id: if id.is_empty() { None } else { Some(id) },
                tool_name: name.clone(),
                tool_arguments_to_pass: input,
            })
            .collect();
        if requested.is_empty() {
            None
        } else {
            tracing::info!(
                "Claude: LLM requested {} tool call(s), executing",
                requested.len()
            );
            if !message_created {
                let payload = TextPayload {
                    text: full_text.clone(),
                    requested_tool_calls: Some(requested.clone()),
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
                    )
                    .await
                    .map_err(|e| format!("ai_message: {}", e))?;
                }
            }
            for r in &requested {
                tracing::info!(tool = %r.tool_name, args = ?r.tool_arguments_to_pass, "Claude: executing tool");
                tools::execute_tool_async(&r.tool_name, r.tool_arguments_to_pass.clone())
                    .await
                    .map_err(|e| format!("Tool execution failed: {}", e))?;
            }
            Some(requested)
        }
    };

    if message_created {
        if let Some(ref conn) = redis_conn {
            let payload = ChatSessionNewMessageChunkPayload::new(
                project_id,
                session_id,
                message_id,
                chunk_index,
                String::new(),
                true,
            );
            let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
            let mut guard = conn.lock().await;
            if let Err(e) = publish_event_to_channel(&mut *guard, topic, &event).await {
                tracing::warn!("Failed to publish final chat chunk to Redis: {}", e);
            }
        }
    }

    Ok(GenerateOutcome::Completed(TextPayload {
        text: full_text,
        requested_tool_calls,
    }))
}
