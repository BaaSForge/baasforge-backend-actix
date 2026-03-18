// TODO: we know that gpt and gemini dont support tool call and content in the same response,
// so we can simplify this code
use crate::models::chat_messages::{ChatAgentType, RequestedToolCall, ChatSessionRoleType, TextPayload};
use crate::services::chat_worker::agent::system_prompt_for_agent;
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::llm::GenerateOutcome;
use futures::StreamExt;
use openai::Credentials;
use openai::chat::{
    ChatCompletion, ChatCompletionChoiceDelta, ChatCompletionDelta, ChatCompletionMessage,
    ChatCompletionMessageDelta, ChatCompletionMessageRole, ToolCall, ToolCallFunction,
};
use redis::aio::MultiplexedConnection;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use super::{BoxError, payload_to_string, tools};
use serde_json::Value;

fn build_chat_messages(
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
    agent: &ChatAgentType,
) -> Vec<ChatCompletionMessage> {
    let system_prompt = system_prompt_for_agent(agent);
    let mut messages: Vec<ChatCompletionMessage> = vec![ChatCompletionMessage {
        role: ChatCompletionMessageRole::System,
        content: Some(system_prompt.to_string()),
        name: None,
        function_call: None,
        tool_call_id: None,
        tool_calls: None,
    }];

    for (role, payload) in session_messages {
        let content = payload_to_string(payload);
        if content.is_empty() {
            continue;
        }
        let role = match role {
            ChatSessionRoleType::User => ChatCompletionMessageRole::User,
            ChatSessionRoleType::Ai => ChatCompletionMessageRole::Assistant,
        };
        messages.push(ChatCompletionMessage {
            role,
            content: Some(content),
            name: None,
            function_call: None,
            tool_call_id: None,
            tool_calls: None,
        });
    }

    messages.push(ChatCompletionMessage {
        role: ChatCompletionMessageRole::User,
        content: Some(user_text.to_string()),
        name: None,
        function_call: None,
        tool_call_id: None,
        tool_calls: None,
    });

    messages
}

/// Build request body for the tools API (supports multiple tool_calls per response).
fn build_tools_request_body(
    model: &str,
    messages: &[ChatCompletionMessage],
    functions: &[openai::chat::ChatCompletionFunctionDefinition],
) -> Value {
    let tools: Vec<Value> = functions
        .iter()
        .map(|f| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": f.name,
                    "description": f.description,
                    "parameters": f.parameters
                }
            })
        })
        .collect();
    serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "tools": tools,
        "tool_choice": "auto"
    })
}

/// Parse a stream chunk JSON into ChatCompletionDelta, using defaults for missing fields
/// so that partial tool_calls (e.g. arguments-only chunks) deserialize correctly.
/// Uses and updates `completion_id` so every delta has the same id (required for merge);
/// later chunks often omit "id", which would otherwise cause merge to fail and drop content.
fn parse_stream_chunk(data: &str, completion_id: &mut String) -> Option<ChatCompletionDelta> {
    let root: Value = serde_json::from_str(data).ok()?;
    if let Some(id) = root.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        *completion_id = id.to_string();
    }
    let choices = root.get("choices")?.as_array()?;
    let mut out_choices = Vec::new();
    for choice in choices {
        let index = choice.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
        let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str()).map(String::from);
        let delta_val = choice.get("delta")?;
        let role = delta_val
            .get("role")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<ChatCompletionMessageRole>(&format!("\"{}\"", s)).ok());
        let content = delta_val.get("content").and_then(|v| v.as_str()).map(String::from);
        let name = delta_val.get("name").and_then(|v| v.as_str()).map(String::from);
        let tool_calls_val = delta_val.get("tool_calls").and_then(|v| v.as_array());
        let tool_calls: Option<Vec<ToolCall>> = tool_calls_val.map(|arr| {
            let indexed: Vec<(u64, ToolCall)> = arr
                .iter()
                .filter_map(|tc| {
                    let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                    let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let ty = tc.get("type").and_then(|v| v.as_str()).unwrap_or("function").to_string();
                    let func = tc.get("function");
                    let (fname, fargs) = func
                        .map(|f| {
                            (
                                f.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                f.get("arguments").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            )
                        })
                        .unwrap_or_default();
                    Some((
                        index,
                        ToolCall {
                            id,
                            r#type: ty,
                            function: ToolCallFunction {
                                name: fname,
                                arguments: fargs,
                            },
                        },
                    ))
                })
                .collect();
            let max_idx = indexed.iter().map(|(i, _)| *i).max().unwrap_or(0);
            let mut out = vec![
                ToolCall {
                    id: String::new(),
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name: String::new(),
                        arguments: String::new(),
                    },
                };
                (max_idx + 1) as usize
            ];
            for (idx, tc) in indexed {
                out[idx as usize] = tc;
            }
            out
        });
        let delta = ChatCompletionMessageDelta {
            role,
            content,
            name,
            function_call: None,
            tool_call_id: None,
            tool_calls,
        };
        out_choices.push(ChatCompletionChoiceDelta {
            index,
            finish_reason,
            delta,
        });
    }
    if out_choices.is_empty() {
        return None;
    }
    let id = if completion_id.is_empty() {
        root.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string()
    } else {
        completion_id.clone()
    };
    Some(ChatCompletionDelta {
        id,
        object: root.get("object").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        created: root.get("created").and_then(|v| v.as_u64()).unwrap_or(0),
        model: root.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        choices: out_choices,
        usage: None,
    })
}

/// Stream chat completion using the tools API (multiple tool_calls per message).
async fn stream_openai_tools(
    credentials: &Credentials,
    model: &str,
    messages: Vec<ChatCompletionMessage>,
    functions: Vec<openai::chat::ChatCompletionFunctionDefinition>,
) -> Result<mpsc::Receiver<ChatCompletionDelta>, BoxError> {
    let body = build_tools_request_body(model, &messages, &functions);
    let url = format!("{}chat/completions", credentials.base_url());
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .header(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", credentials.api_key()))
                .map_err(|e| format!("Invalid auth header: {}", e))?,
        )
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("OpenAI tools stream request: {}", e))?;
    if !response.status().is_success() {
        let status = response.status();
        let err_body = response.text().await.unwrap_or_default();
        return Err(format!("OpenAI tools stream error {}: {}", status, err_body).into());
    }
    let (tx, rx) = mpsc::channel::<ChatCompletionDelta>(32);
    let mut stream = response.bytes_stream();
    tokio::spawn(async move {
        let mut buf = Vec::new();
        let mut completion_id = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("OpenAI stream chunk error: {}", e);
                    break;
                }
            };
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        continue;
                    }
                    if let Some(delta) = parse_stream_chunk(data, &mut completion_id) {
                        if tx.send(delta).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
        if !buf.is_empty() {
            let line = String::from_utf8_lossy(&buf).trim().to_string();
            if let Some(data) = line.strip_prefix("data: ") {
                if data != "[DONE]" {
                    if let Some(delta) = parse_stream_chunk(data, &mut completion_id) {
                        let _ = tx.send(delta).await;
                    }
                }
            }
        }
    });
    Ok(rx)
}

async fn stream_content_to_redis(
    content: &str,
    chunk_index: &mut u32,
    redis_conn: Option<&Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
    message_id: uuid::Uuid,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
) -> Result<(), ()> {
    if content.is_empty() {
        return Ok(());
    }
    if let Some(ref conn) = redis_conn {
        let mut guard = conn.lock().await;
        if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
            return Err(());
        }
        let payload = ChatSessionNewMessageChunkPayload::new(
            project_id,
            session_id,
            message_id,
            *chunk_index,
            content.to_string(),
            false,
        );
        let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
        if let Err(e) = publish_event_to_channel(&mut *guard, topic, &event).await {
            tracing::warn!("Failed to publish chat chunk to Redis: {}", e);
        }
        *chunk_index += 1;
    }
    Ok(())
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
    let functions = tools::to_openai_functions();
    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;
    let mut message_created = false;

    let mut rx = stream_openai_tools(credentials, model, messages, functions)
        .await
        .map_err(|e| format!("OpenAI stream: {}", e))?;

    let mut merged: Option<ChatCompletionDelta> = None;
    /// (tool_call_id, tool_name, arguments_json)
    let mut accumulated_tool_calls: Vec<(String, String, String)> = Vec::new();

    tracing::info!(
        tools_defined = tools::all_tool_specs().len(),
        "OpenAI: stream started with tools attached"
    );

    while let Some(delta) = rx.recv().await {
        for choice in &delta.choices {
            // Debug: log whether this delta has content and/or tool_calls (GPT can send both in same response)
            let content_len = choice
                .delta
                .content
                .as_deref()
                .map(|s| s.len())
                .unwrap_or(0);
            let tool_calls_in_delta = choice
                .delta
                .tool_calls
                .as_ref()
                .map(|tcs| tcs.len())
                .unwrap_or(0);
            if content_len > 0 || tool_calls_in_delta > 0 {
                // tracing::debug!(
                //     delta_content_len = content_len,
                //     delta_tool_calls_count = tool_calls_in_delta,
                //     "OpenAI delta: content and/or tool_calls in this chunk"
                // );
            }
            if let Some(ref content) = choice.delta.content {
                if !content.is_empty() {
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
                        .map_err(|e| -> BoxError { e.into() })?;
                    }
                    message_created = true;
                    full_text.push_str(content);
                    if stream_content_to_redis(
                        content,
                        &mut chunk_index,
                        redis_conn.as_ref(),
                        topic,
                        message_id,
                        project_id,
                        session_id,
                    )
                    .await
                    .is_err()
                    {
                        return Ok(GenerateOutcome::Stopped(TextPayload {
                        text: full_text,
                        requested_tool_calls: None,
                    }));
                    }
                }
            }
            if let Some(ref tcs) = choice.delta.tool_calls {
                for (i, tc) in tcs.iter().enumerate() {
                    if accumulated_tool_calls.len() <= i {
                        accumulated_tool_calls.resize(i + 1, (String::new(), String::new(), String::new()));
                    }
                    let acc = &mut accumulated_tool_calls[i];
                    if !tc.id.is_empty() {
                        acc.0 = tc.id.clone();
                    }
                    if !tc.function.name.is_empty() {
                        acc.1 = tc.function.name.clone();
                    }
                    acc.2.push_str(&tc.function.arguments);
                }
            }
            // Legacy API: stream sends function_call in delta (not tool_calls); accumulate it too.
            // Multiple calls: when we see a new function name, start a new slot instead of merging into the first.
            if let Some(ref fc) = choice.delta.function_call {
                let name = fc.name.as_deref().unwrap_or("");
                let args = fc.arguments.as_deref().unwrap_or("");
                if !name.is_empty() || !args.is_empty() {
                    let last_name = accumulated_tool_calls
                        .last()
                        .map(|t| t.1.as_str())
                        .unwrap_or("");
                    let is_new_call = !name.is_empty() && name != last_name;
                    if accumulated_tool_calls.is_empty() || is_new_call {
                        accumulated_tool_calls.push((String::new(), name.to_string(), args.to_string()));
                    } else {
                        let last = accumulated_tool_calls.last_mut().unwrap();
                        if !name.is_empty() {
                            last.1 = name.to_string();
                        }
                        last.2.push_str(args);
                    }
                }
            }
        }
        match &mut merged {
            Some(c) => {
                c.merge(delta)
                    .map_err(|e| format!("OpenAI stream merge: {:?}", e))?;
            }
            None => merged = Some(delta),
        }
    }

    let mut requested_tool_calls_out: Option<Vec<RequestedToolCall>> = None;
    if let Some(completion) = merged {
        // Read merged content from the delta before converting to ChatCompletion (so we have
        // the full streamed text when the model sent both content and tool_calls).
        let merged_content = completion
            .choices
            .first()
            .and_then(|c| c.delta.content.clone())
            .unwrap_or_default();

        let chat = ChatCompletion::from(completion);
        let (finish_reason, message) = chat
            .choices
            .into_iter()
            .next()
            .map(|c| (c.finish_reason.as_str().to_string(), Some(c.message)))
            .unwrap_or_else(|| (String::new(), None));

        // Debug: compare stream-accumulated text vs merged delta content (merge may not concatenate content)
        let stream_accumulated_len = full_text.len();
        tracing::info!(
            finish_reason = %finish_reason,
            tool_calls_made = accumulated_tool_calls.len(),
            stream_accumulated_content_len = stream_accumulated_len,
            merged_delta_content_len = merged_content.len(),
            "OpenAI: stream ended, checking for tool call"
        );

        // Legacy API streams function_call in deltas (crate merges it); new API uses tool_calls.
        // finish_reason can be empty even when the model called a tool, so also check message.
        let message_has_function_call = message
            .as_ref()
            .and_then(|m| m.function_call.as_ref())
            .is_some();
        let has_tool_calls = !accumulated_tool_calls.is_empty()
            || finish_reason.as_str() == "tool_calls"
            || finish_reason.as_str() == "function_call"
            || message_has_function_call;

        if has_tool_calls {
            let to_run: Vec<(String, String, String)> = if accumulated_tool_calls.is_empty() {
                if let Some(ref msg) = message {
                    if let Some(ref fc) = msg.function_call {
                        vec![(String::new(), fc.name.clone(), fc.arguments.clone())]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            } else {
                accumulated_tool_calls
            };

            requested_tool_calls_out = Some(
                to_run
                    .iter()
                    .filter(|(_, name, _)| !name.is_empty())
                    .map(|(id, name, arguments)| {
                        let tool_arguments_to_pass: Value = serde_json::from_str(arguments)
                            .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
                        RequestedToolCall {
                            tool_call_id: if id.is_empty() { None } else { Some(id.clone()) },
                            tool_name: name.clone(),
                            tool_arguments_to_pass,
                        }
                    })
                    .collect(),
            );

            // Publish invoker message to client before executing tools so client sees correct order.
            if !message_created {
                let invoker_payload = TextPayload {
                    text: full_text.clone(),
                    requested_tool_calls: requested_tool_calls_out.clone(),
                };
                if let Some(ref conn) = redis_conn {
                    ai_message::create_ai_message_with_payload_and_publish(
                        pool,
                        conn,
                        session_id,
                        message_id,
                        user_id,
                        project_id,
                        &invoker_payload,
                        agent.clone(),
                    )
                    .await
                    .map_err(|e| -> BoxError { e.into() })?;
                }
            }

            for (_, name, arguments) in &to_run {
                if name.is_empty() {
                    continue;
                }
                let args: Value = serde_json::from_str(arguments)
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                tracing::info!(tool = %name, args = %arguments, "OpenAI requested tool call, executing");
                tools::execute_tool_async(name, args).await?;
            }
            // GPT can send both content and tool_calls in the same response. Prefer stream-accumulated
            // full_text (we built it in the loop); the crate's merge() often keeps only the last
            // content delta, so merged_content can be empty or partial.
            if !full_text.is_empty() {
                tracing::debug!(
                    len = full_text.len(),
                    "OpenAI: using stream-accumulated content as message (tool_calls in same response)"
                );
                // full_text already has the right value
            } else if !merged_content.is_empty() {
                full_text = merged_content;
            } else if let Some(ref m) = message {
                let msg_content = m.content.clone().unwrap_or_default();
                if !msg_content.is_empty() {
                    full_text = msg_content;
                }
            }
        } else {
            tracing::info!(
                "OpenAI: no tool call (finish_reason != function_call/tool_calls), using text response"
            );
            if let Some(m) = message {
                full_text = m.content.unwrap_or_default();
            }
        }
    } else {
        tracing::info!("OpenAI: stream ended with no completion (merged was None)");
    }

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
        requested_tool_calls: requested_tool_calls_out,
    }))
}
