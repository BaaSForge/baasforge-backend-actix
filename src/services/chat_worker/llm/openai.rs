use crate::models::chat_messages::{ChatAgentType, RequestedToolCall, ChatSessionRoleType, TextPayload};
use crate::services::chat_worker::agent::system_prompt_for_agent;
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::llm::GenerateOutcome;
use futures::StreamExt;
use openai::Credentials;
use openai::chat::{ChatCompletionMessage, ChatCompletionMessageRole};
use redis::aio::MultiplexedConnection;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use super::{BoxError, payload_to_string, tools};
use crate::services::chat_worker::agent::tool_names_for_agent;
use serde_json::Value;

struct StreamChunk {
    content: Option<String>,
    tool_calls: Option<Vec<(u64, String, String, String)>>, // index, id, name, args
    function_call: Option<(String, String)>, // legacy: name, args
    finish_reason: Option<String>,
}

fn openai_msg(role: ChatCompletionMessageRole, content: impl Into<String>) -> ChatCompletionMessage {
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
    let names = tool_names_for_agent(agent);
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

fn build_chat_messages(
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
    agent: &ChatAgentType,
) -> Vec<ChatCompletionMessage> {
    let mut messages = vec![openai_msg(
        ChatCompletionMessageRole::System,
        system_prompt_for_agent(agent),
    )];
    for (role, payload) in session_messages {
        let c = payload_to_string(payload);
        if c.is_empty() {
            continue;
        }
        let role = match role {
            ChatSessionRoleType::User => ChatCompletionMessageRole::User,
            ChatSessionRoleType::Ai => ChatCompletionMessageRole::Assistant,
        };
        messages.push(openai_msg(role, c));
    }
    messages.push(openai_msg(ChatCompletionMessageRole::User, user_text));
    messages
}

fn parse_sse_chunk(data: &str) -> Option<StreamChunk> {
    let root: Value = serde_json::from_str(data).ok()?;
    let delta = root.get("choices")?.as_array()?.first()?.get("delta")?;
    let content = delta.get("content").and_then(|v| v.as_str()).map(String::from);
    let finish_reason = root.get("choices")?.as_array()?.first()?.get("finish_reason").and_then(|v| v.as_str()).map(String::from);
    let tool_calls = delta.get("tool_calls").and_then(|v| v.as_array()).map(|arr| {
        arr.iter().filter_map(|tc| {
            let func = tc.get("function");
            Some((
                tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0),
                tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                func.and_then(|f| f.get("name").and_then(|v| v.as_str())).unwrap_or("").to_string(),
                func.and_then(|f| f.get("arguments").and_then(|v| v.as_str())).unwrap_or("").to_string(),
            ))
        }).collect()
    });
    let function_call = delta.get("function_call").filter(|fc| fc.is_object()).map(|fc| (
        fc.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        fc.get("arguments").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    ));
    Some(StreamChunk { content, tool_calls, function_call, finish_reason })
}

async fn stream_openai_tools(
    credentials: &Credentials,
    model: &str,
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
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "tools": tools_json,
        "tool_choice": "auto"
    });
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
                if let Some(data) = line.strip_prefix("data: ").filter(|d| *d != "[DONE]") {
                    if let Some(d) = parse_sse_chunk(data) {
                        let _ = tx.send(d).await;
                    }
                }
            }
        }
        if !buf.is_empty() {
            let lossy = String::from_utf8_lossy(&buf);
            let line = lossy.trim();
            if let Some(data) = line.strip_prefix("data: ").filter(|d| *d != "[DONE]") {
                if let Some(d) = parse_sse_chunk(data) {
                    let _ = tx.send(d).await;
                }
            }
        }
    });
    Ok(rx)
}

pub async fn call_gpt_stream(
    credentials: &Credentials,
    model: &str,
    pool: &crate::database::DbPool,
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
    message_id: uuid::Uuid,
    user_id: uuid::Uuid,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    agent: ChatAgentType,
) -> Result<GenerateOutcome, BoxError> {
    let messages = build_chat_messages(session_messages, user_text, &agent);
    let functions = to_openai_functions_for_agent(&agent);
    let mut rx = stream_openai_tools(credentials, model, messages, &functions)
        .await
        .map_err(|e| format!("OpenAI stream: {}", e))?;

    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;
    let mut message_created = false;
    let mut is_tool_response = false;
    let mut acc: Vec<(String, String, String)> = Vec::new(); // id, name, args
    let mut last_finish_reason = String::new();

    tracing::info!(tools = functions.len(), "OpenAI stream started");

    while let Some(chunk) = rx.recv().await {
        if chunk.tool_calls.is_some() || chunk.function_call.is_some() {
            is_tool_response = true;
        }
        if let Some(c) = chunk.content.as_ref().filter(|s| !s.is_empty()) {
            full_text.push_str(c);
            if !is_tool_response {
                if let Some(ref conn) = redis_conn {
                    ai_message::ensure_ai_message_created_and_publish(
                        pool, conn, session_id, message_id, user_id, project_id, agent.clone(),
                    ).await?;
                    let mut guard = conn.lock().await;
                    if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                        return Ok(GenerateOutcome::Stopped(TextPayload {
                            text: full_text, requested_tool_calls: None,
                        }));
                    }
                    let payload = ChatSessionNewMessageChunkPayload::new(
                        project_id, session_id, message_id, chunk_index, c.clone(), false,
                    );
                    let _ = publish_event_to_channel(&mut *guard, topic, &WsServerEvent::ChatSessionNewMessageChunk(payload)).await;
                    chunk_index += 1;
                }
                message_created = true;
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
        if let Some((name, args)) = &chunk.function_call {
            if !name.is_empty() || !args.is_empty() {
                let last_name = acc.last().map(|t| t.1.as_str()).unwrap_or("");
                if acc.is_empty() || (!name.is_empty() && name.as_str() != last_name) {
                    acc.push((String::new(), name.clone(), args.clone()));
                } else {
                    let last = acc.last_mut().unwrap();
                    if !name.is_empty() { last.1 = name.clone(); }
                    last.2.push_str(args);
                }
            }
        }
        if let Some(r) = chunk.finish_reason {
            last_finish_reason = r;
        }
    }

    let has_tool_calls = !acc.is_empty()
        || last_finish_reason == "tool_calls"
        || last_finish_reason == "function_call";

    let requested_tool_calls_out: Option<Vec<RequestedToolCall>> = if has_tool_calls {
        let to_run: Vec<_> = acc.into_iter().filter(|(_, name, _)| !name.is_empty()).collect();
        let requested: Vec<RequestedToolCall> = to_run.iter().map(|(id, name, arguments)| {
            let args = serde_json::from_str(arguments).unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
            RequestedToolCall {
                tool_call_id: if id.is_empty() { None } else { Some(id.clone()) },
                tool_name: name.clone(),
                tool_arguments_to_pass: args,
            }
        }).collect();
        if !message_created {
            let payload = TextPayload { text: full_text.clone(), requested_tool_calls: Some(requested.clone()) };
            if let Some(ref conn) = redis_conn {
                ai_message::create_ai_message_with_payload_and_publish(
                    pool, conn, session_id, message_id, user_id, project_id, &payload, agent.clone(),
                ).await?;
            }
        }
        for (_, name, arguments) in &to_run {
            let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
            tools::execute_tool_async(name, args).await?;
        }
        Some(requested)
    } else {
        None
    };

    if message_created {
        if let Some(ref conn) = redis_conn {
            let payload = ChatSessionNewMessageChunkPayload::new(
                project_id, session_id, message_id, chunk_index, String::new(), true,
            );
            let mut guard = conn.lock().await;
            let _ = publish_event_to_channel(&mut *guard, topic, &WsServerEvent::ChatSessionNewMessageChunk(payload)).await;
        }
    }

    Ok(GenerateOutcome::Completed(TextPayload {
        text: full_text,
        requested_tool_calls: requested_tool_calls_out,
    }))
}
