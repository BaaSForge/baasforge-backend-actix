// OpenAI Chat Completions via HTTP (reqwest + serde_json). Uses `openai` crate types for messages only.
// Reasoning: request field `reasoning_effort` (Chat Completions API) — see
// https://platform.openai.com/docs/guides/reasoning and
// https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create
// Streamed model reasoning may appear on chunk `choices[].delta.reasoning_content` when the model/API exposes it
// (not all models stream visible reasoning; raw reasoning tokens are often omitted — use Responses API + summaries for that).
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
use futures::StreamExt;
use openai::Credentials;
use redis::aio::MultiplexedConnection;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use super::{BoxError, tools};
use crate::services::chat_worker::llm::history::{
    session_messages_to_upstream_entries, UpstreamHistoryEntry,
};
use openai::chat::{ChatCompletionMessage, ChatCompletionMessageRole, ToolCall, ToolCallFunction};
use serde_json::Value;

struct StreamChunk {
    content: Option<String>,
    /// Streamed chain-of-thought / reasoning text when the API includes it on the delta (e.g. some reasoning models).
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<(u64, String, String, String)>>, // index, id, name, args
    function_call: Option<(String, String)>,                // legacy: name, args
    finish_reason: Option<String>,
}

fn thinking_blocks_from_accumulated_reasoning(reasoning: &str) -> Option<Vec<ThinkingBlock>> {
    if reasoning.is_empty() {
        None
    } else {
        Some(vec![ThinkingBlock {
            thinking: reasoning.to_string(),
            signature: None,
        }])
    }
}

fn openai_msg(
    role: ChatCompletionMessageRole,
    content: impl Into<String>,
) -> ChatCompletionMessage {
    ChatCompletionMessage {
        role,
        content: Some(content.into()),
        name: None,
        function_call: None,
        tool_call_id: None,
        tool_calls: None,
    }
}

fn to_openai_functions_for_agent(
    agent: &ChatAgentType,
) -> Vec<openai::chat::ChatCompletionFunctionDefinition> {
    let names = tools::tool_names_for_llm_session(agent);
    tools::all_tool_specs()
        .into_iter()
        .filter(|t| names.contains(&t.name))
        .map(|t| openai::chat::ChatCompletionFunctionDefinition {
            name: t.name.to_string(),
            description: Some(t.description.to_string()),
            parameters: Some(t.parameters),
        })
        .collect()
}

/// Convert upstream history entries to OpenAI Chat Completion messages.
/// Tool-call request and tool results are always separate messages (never combined):
/// - One Assistant message with tool_calls (the request).
/// - Then one Tool-role message per result, each with its own tool_call_id and content.
///
/// Stored `thinking_blocks` from prior turns are not re-sent: unlike Anthropic extended thinking,
/// Chat Completions has no stable assistant-message shape for raw reasoning replay; reasoning
/// context is model-internal. We still persist thinking in DB / WS for the UI.
fn entries_to_openai_messages(
    entries: &[UpstreamHistoryEntry],
    system_prompt: &str,
) -> Vec<ChatCompletionMessage> {
    let mut messages = vec![openai_msg(
        ChatCompletionMessageRole::System,
        system_prompt,
    )];
    for entry in entries {
        match entry {
            UpstreamHistoryEntry::User { text } => {
                if !text.is_empty() {
                    messages.push(openai_msg(ChatCompletionMessageRole::User, text.clone()));
                }
            }
            UpstreamHistoryEntry::Assistant { text, tool_calls, .. } => {
                let content = if text.trim().is_empty() {
                    None
                } else {
                    Some(text.trim().to_string())
                };
                let tool_calls: Option<Vec<ToolCall>> = tool_calls.as_ref().and_then(|tcs| {
                    let v: Vec<ToolCall> = tcs
                        .iter()
                        .filter(|tc| !tc.name.is_empty())
                        .map(|tc| {
                            let arguments = serde_json::to_string(&tc.arguments)
                                .unwrap_or_else(|_| "{}".to_string());
                            ToolCall {
                                id: tc.id.clone(),
                                r#type: "function".to_string(),
                                function: ToolCallFunction {
                                    name: tc.name.clone(),
                                    arguments,
                                },
                            }
                        })
                        .collect();
                    if v.is_empty() {
                        None
                    } else {
                        Some(v)
                    }
                });
                if content.is_some() || tool_calls.is_some() {
                    messages.push(ChatCompletionMessage {
                        role: ChatCompletionMessageRole::Assistant,
                        content,
                        name: None,
                        function_call: None,
                        tool_call_id: None,
                        tool_calls,
                    });
                }
            }
            UpstreamHistoryEntry::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                messages.push(ChatCompletionMessage {
                    role: ChatCompletionMessageRole::Tool,
                    content: Some(content.clone()),
                    name: None,
                    function_call: None,
                    tool_call_id: Some(tool_call_id.clone()),
                    tool_calls: Some(Vec::new()),
                });
            }
        }
    }
    messages
}

fn parse_sse_chunk(data: &str) -> Option<StreamChunk> {
    let root: Value = serde_json::from_str(data).ok()?;
    let choice = root.get("choices")?.as_array()?.first()?;
    let delta = choice.get("delta")?;
    let content = delta
        .get("content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let reasoning_content = delta
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .map(String::from);
    let tool_calls = delta
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let func = tc.get("function");
                    Some((
                        tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0),
                        tc.get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        func.and_then(|f| f.get("name").and_then(|v| v.as_str()))
                            .unwrap_or("")
                            .to_string(),
                        func.and_then(|f| f.get("arguments").and_then(|v| v.as_str()))
                            .unwrap_or("")
                            .to_string(),
                    ))
                })
                .collect()
        });
    let function_call = delta
        .get("function_call")
        .filter(|fc| fc.is_object())
        .map(|fc| {
            (
                fc.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                fc.get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        });
    if content.is_none()
        && reasoning_content.is_none()
        && tool_calls.is_none()
        && function_call.is_none()
        && finish_reason.is_none()
    {
        return None;
    }
    Some(StreamChunk {
        content,
        reasoning_content,
        tool_calls,
        function_call,
        finish_reason,
    })
}

async fn stream_openai_tools(
    credentials: &Credentials,
    model: &str,
    reasoning_effort: Option<&str>,
    messages: Vec<ChatCompletionMessage>,
    functions: &[openai::chat::ChatCompletionFunctionDefinition],
) -> Result<mpsc::Receiver<StreamChunk>, BoxError> {
    let tools_json: Vec<Value> = functions
        .iter()
        .map(|f| {
            serde_json::json!({
                "type": "function",
                "function": { "name": f.name, "description": f.description, "parameters": f.parameters }
            })
        })
        .collect();
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "tools": tools_json,
        "tool_choice": "auto"
    });
    if let Some(effort) = reasoning_effort.map(str::trim).filter(|s| !s.is_empty())
        && let Some(obj) = body.as_object_mut() {
            obj.insert(
                "reasoning_effort".to_string(),
                Value::String(effort.to_string()),
            );
        }
    let url = format!("{}chat/completions", credentials.base_url());
    let response = reqwest::Client::new()
        .post(&url)
        .header(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", credentials.api_key()))
                .map_err(|e| format!("Invalid auth: {}", e))?,
        )
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("OpenAI request: {}", e))?;
    if !response.status().is_success() {
        let status = response.status();
        let err = response.text().await.unwrap_or_default();
        return Err(format!("OpenAI error {}: {}", status, err).into());
    }
    let (tx, rx) = mpsc::channel::<StreamChunk>(32);
    let mut stream = response.bytes_stream();
    tokio::spawn(async move {
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("OpenAI stream: {}", e);
                    break;
                }
            };
            buf.extend_from_slice(&bytes);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let drained: Vec<u8> = buf.drain(..=pos).collect();
                let lossy = String::from_utf8_lossy(&drained);
                let line = lossy.trim();
                if let Some(data) = line.strip_prefix("data: ").filter(|d| *d != "[DONE]")
                    && let Some(d) = parse_sse_chunk(data)
                {
                    let _ = tx.send(d).await;
                }
            }
        }
        if !buf.is_empty() {
            let lossy = String::from_utf8_lossy(&buf);
            let line = lossy.trim();
            if let Some(data) = line.strip_prefix("data: ").filter(|d| *d != "[DONE]")
                && let Some(d) = parse_sse_chunk(data)
            {
                let _ = tx.send(d).await;
            }
        }
    });
    Ok(rx)
}

pub async fn call_gpt_stream(
    credentials: &Credentials,
    model: &str,
    reasoning_effort: Option<&str>,
    pool: &crate::database::DbPool,
    session_messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    user_text: &str,
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
    message_id: uuid::Uuid,
    user_id: uuid::Uuid,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    agent: ChatAgentType,
    queue_invoking_message_id: Option<uuid::Uuid>,
    stale_base_release: bool,
) -> Result<GenerateOutcome, BoxError> {
    let entries = session_messages_to_upstream_entries(session_messages, user_text);
    let system_prompt = system_prompt_for_agent(&agent, stale_base_release);
    let messages = entries_to_openai_messages(&entries, system_prompt.as_ref());
    let functions = to_openai_functions_for_agent(&agent);
    let mut rx = stream_openai_tools(
        credentials,
        model,
        reasoning_effort,
        messages,
        &functions,
    )
    .await
    .map_err(|e| format!("OpenAI stream: {}", e))?;

    let mut full_text = String::new();
    let mut full_thinking = String::new();
    let mut chunk_index: u32 = 0;
    let mut thinking_chunk_index: u32 = 0;
    let mut is_tool_response = false;
    let mut acc: Vec<(String, String, String)> = Vec::new(); // id, name, args
    let mut last_finish_reason = String::new();

    tracing::info!(
        tools = functions.len(),
        model,
        ?reasoning_effort,
        "OpenAI stream started"
    );

    while let Some(chunk) = rx.recv().await {
        if chunk.tool_calls.is_some() || chunk.function_call.is_some() {
            is_tool_response = true;
        }
        if let Some(r) = chunk.reasoning_content.as_ref().filter(|s| !s.is_empty()) {
            full_thinking.push_str(r);
            if let Some(ref conn) = redis_conn {
                let mut guard = conn.lock().await;
                if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                    return Ok(GenerateOutcome::Stopped(TextPayload {
                        text: full_text,
                        requested_tool_calls: None,
                        thinking_blocks: thinking_blocks_from_accumulated_reasoning(&full_thinking),
                    }));
                }
                let payload = ChatSessionNewThinkingChunkPayload::new(
                    project_id,
                    session_id,
                    message_id,
                    thinking_chunk_index,
                    r.clone(),
                    false,
                );
                let ws_event = WsServerEvent::ChatSessionNewThinkingChunk(payload);
                if let Err(e) = publish_event_to_channel(&mut *guard, topic, &ws_event).await {
                    tracing::warn!("Failed to publish OpenAI thinking chunk to Redis: {}", e);
                }
                thinking_chunk_index += 1;
            }
        }
        if let Some(c) = chunk.content.as_ref().filter(|s| !s.is_empty()) {
            full_text.push_str(c);
            if !is_tool_response
                && let Some(ref conn) = redis_conn {
                    let mut guard = conn.lock().await;
                    if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                        return Ok(GenerateOutcome::Stopped(TextPayload {
                            text: full_text,
                            requested_tool_calls: None,
                            thinking_blocks: thinking_blocks_from_accumulated_reasoning(&full_thinking),
                        }));
                    }
                    let payload = ChatSessionNewMessageChunkPayload::new(
                        project_id,
                        session_id,
                        message_id,
                        chunk_index,
                        c.clone(),
                        false,
                    );
                    let _ = publish_event_to_channel(
                        &mut *guard,
                        topic,
                        &WsServerEvent::ChatSessionNewMessageChunk(payload),
                    )
                    .await;
                    chunk_index += 1;
                }
        }
        if let Some(ref tcs) = chunk.tool_calls {
            for (idx, id, name, args) in tcs {
                let i = *idx as usize;
                if acc.len() <= i {
                    acc.resize(i + 1, (String::new(), String::new(), String::new()));
                }
                let a = &mut acc[i];
                if !id.is_empty() {
                    a.0 = id.clone();
                }
                if !name.is_empty() {
                    a.1 = name.clone();
                }
                a.2.push_str(args);
            }
        }
        if let Some((name, args)) = &chunk.function_call
            && (!name.is_empty() || !args.is_empty())
        {
            let last_name = acc.last().map(|t| t.1.as_str()).unwrap_or("");
            if acc.is_empty() || (!name.is_empty() && name.as_str() != last_name) {
                acc.push((String::new(), name.clone(), args.clone()));
            } else {
                let last = acc.last_mut().unwrap();
                if !name.is_empty() {
                    last.1 = name.clone();
                }
                last.2.push_str(args);
            }
        }
        if let Some(r) = chunk.finish_reason {
            last_finish_reason = r;
        }
    }

    let has_tool_calls = !acc.is_empty()
        || last_finish_reason == "tool_calls"
        || last_finish_reason == "function_call";

    let had_content_stream = !full_text.is_empty() || !full_thinking.is_empty();
    let thinking_blocks = thinking_blocks_from_accumulated_reasoning(&full_thinking);
    let published_stream_chunks = chunk_index > 0 || thinking_chunk_index > 0;

    let requested_tool_calls_out: Option<Vec<RequestedToolCall>> = if has_tool_calls {
        let to_run: Vec<_> = acc
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .collect();
        let requested: Vec<RequestedToolCall> = to_run
            .iter()
            .map(|(id, name, arguments)| {
                let args = serde_json::from_str(arguments)
                    .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
                RequestedToolCall {
                    tool_call_id: if id.is_empty() {
                        None
                    } else {
                        Some(id.clone())
                    },
                    tool_name: name.clone(),
                    tool_arguments_to_pass: args,
                    thought_signature: None,
                }
            })
            .collect();
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
                .await?;
            }
        }
        for (id, name, arguments) in &to_run {
            let args: Value = serde_json::from_str(arguments)
                .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
            let tool_call_id = if id.is_empty() {
                None
            } else {
                Some(id.clone())
            };
            if let Err(e) = tools::execute_tool_async(name, args, tool_call_id).await {
                tracing::warn!(
                    tool = %name,
                    "Tool arg validation failed (Failed row persisted); continuing remaining tool calls: {}",
                    e
                );
            }
        }
        Some(requested)
    } else {
        None
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
            let _ = publish_event_to_channel(
                &mut *guard,
                topic,
                &WsServerEvent::ChatSessionNewThinkingChunk(thinking_done),
            )
            .await;
        }
        // Close assistant text stream (including thinking-only turns: chunk_index may be 0).
        let payload = ChatSessionNewMessageChunkPayload::new(
            project_id,
            session_id,
            message_id,
            chunk_index,
            String::new(),
            true,
        );
        let _ = publish_event_to_channel(
            &mut *guard,
            topic,
            &WsServerEvent::ChatSessionNewMessageChunk(payload),
        )
        .await;
    }

    Ok(GenerateOutcome::Completed(TextPayload {
        text: full_text,
        requested_tool_calls: requested_tool_calls_out,
        thinking_blocks,
    }))
}
