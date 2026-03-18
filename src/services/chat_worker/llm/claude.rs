// Claude (Anthropic): streaming and tool use via pure HTTP (reqwest + serde_json).
// No third-party Anthropic crate; request/response follow official API.
// https://docs.anthropic.com/en/api/messages
//
// Replacing anthropic_rust with pure HTTP added ~130 lines of our own code:
//   - Request: build_request_body, message_json, build_messages, to_tools_json (~55 lines)
//   - SSE: StreamEvent enum + read_one_sse_event parsing (~75 lines)
// Total file ~396 lines; dependency removed from Cargo.toml.
use crate::models::chat_messages::{
    ChatAgentType, ChatSessionRoleType, RequestedToolCall, TextPayload,
};
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::services::chat_worker::agent::system_prompt_for_agent;
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::llm::GenerateOutcome;
use futures::StreamExt;
use redis::aio::MultiplexedConnection;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::tools;
use super::{BoxError, payload_to_string};
use crate::services::chat_worker::agent::tool_names_for_agent;
use serde_json::Value;

// ---- Request/response types (no crate) ----

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 8192;

/// Official API model IDs (https://docs.anthropic.com/en/docs/about-claude/models/overview).
fn api_model_id(config_model: &str) -> &'static str {
    match config_model {
        "claude-sonnet-4-6" => "claude-sonnet-4-6",
        "claude-sonnet-4-5" | "claude-sonnet-4-5-20250929" => "claude-sonnet-4-5-20250929",
        "claude-4-sonnet-20250514" | "claude-sonnet-4-20250514" => "claude-sonnet-4-20250514",
        _ => "claude-sonnet-4-20250514",
    }
}

/// Build request body for POST /v1/messages (streaming, with tools).
fn build_request_body(
    system_prompt: &str,
    messages: Vec<Value>,
    tools_json: Vec<Value>,
    api_model: &str,
) -> Value {
    serde_json::json!({
        "model": api_model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": system_prompt,
        "messages": messages,
        "tools": tools_json,
    })
}

/// One message: { "role": "user"|"assistant", "content": [ { "type": "text", "text": "..." } ] }.
fn message_json(role: &str, text: &str) -> Value {
    serde_json::json!({
        "role": role,
        "content": [{ "type": "text", "text": text }],
    })
}

fn build_messages(
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
) -> Vec<Value> {
    let mut out = Vec::new();
    for (role, payload) in session_messages {
        let c = payload_to_string(payload).trim().to_string();
        if c.is_empty() {
            continue;
        }
        let role_str = match role {
            ChatSessionRoleType::User => "user",
            ChatSessionRoleType::Ai => "assistant",
        };
        out.push(message_json(role_str, &c));
    }
    let user_text = user_text.trim();
    if !user_text.is_empty() {
        out.push(message_json("user", user_text));
    }
    out
}

fn to_tools_json(agent: &ChatAgentType) -> Vec<Value> {
    let names = tool_names_for_agent(agent);
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

// ---- SSE parsing (no crate) ----

/// Events we care about from the stream (API sends message_start, content_block_start, content_block_delta, message_stop, etc.).
enum StreamEvent {
    TextDelta {
        text: String,
    },
    ToolUseStart {
        index: usize,
        id: String,
        name: String,
        input: Value,
    },
    InputJsonDelta {
        index: usize,
        partial_json: String,
    },
    MessageStop,
}

/// Read one SSE "data: {...}" line from the stream.
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
                        if delta.get("type").and_then(|t| t.as_str()) == Some("input_json_delta") {
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
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                            return Ok(Some(StreamEvent::TextDelta {
                                text: text.to_string(),
                            }));
                        }
                    }
                    continue;
                }

                if event_type == Some("content_block_start") {
                    let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    if let Some(block) = v.get("content_block")
                        && block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    {
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
                        return Ok(Some(StreamEvent::ToolUseStart {
                            index,
                            id,
                            name,
                            input,
                        }));
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

// ---- Main entry ----

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
    let tools_list = to_tools_json(&agent);
    let tool_count = tools_list.len();
    let api_model = api_model_id(model);

    let messages = build_messages(session_messages, user_text);
    if messages.is_empty() {
        return Err(
            "Claude: no messages to send (session history and user text are empty or whitespace)"
                .into(),
        );
    }

    let body = build_request_body(system_prompt, messages, tools_list.clone(), api_model);

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

    let mut stream = response
        .bytes_stream()
        .map(|r| r.map(|b| b.to_vec()).map_err(|e| e.to_string()));
    let mut buf: Vec<u8> = Vec::new();
    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;
    let mut message_created = false;
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
            StreamEvent::TextDelta { text } => {
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
                        if let Err(e) =
                            publish_event_to_channel(&mut *guard, topic, &ws_event).await
                        {
                            tracing::warn!("Failed to publish chat chunk to Redis: {}", e);
                        }
                        chunk_index += 1;
                    }
                }
            }
            StreamEvent::ToolUseStart {
                index,
                id,
                name,
                input,
            } => {
                tool_use_by_index.insert(*index, (id.clone(), name.clone(), input.clone()));
            }
            StreamEvent::InputJsonDelta {
                index,
                partial_json,
            } => {
                accumulated_tool_input
                    .entry(*index)
                    .or_default()
                    .push_str(partial_json);
            }
            StreamEvent::MessageStop => break,
        }
    }

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

    if message_created && let Some(ref conn) = redis_conn {
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

    Ok(GenerateOutcome::Completed(TextPayload {
        text: full_text,
        requested_tool_calls,
    }))
}
