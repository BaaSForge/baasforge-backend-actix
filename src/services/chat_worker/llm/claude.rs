// Claude (Anthropic) via anthropic_rust crate: streaming and tool use.
use crate::models::chat_messages::{ChatAgentType, RequestedToolCall, ChatSessionRoleType, TextPayload};
use crate::services::chat_worker::agent::system_prompt_for_agent;
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::llm::GenerateOutcome;
use anthropic_rust::{Client, ContentBlock, Model, Role, StreamEvent, Tool};
use futures::StreamExt;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::tools;
use super::{payload_to_string, BoxError};
use crate::services::chat_worker::agent::tool_names_for_agent;
use serde_json::Value;

fn model_from_str(s: &str) -> Model {
    match s {
        "claude-3-haiku-20240307" | "Claude3Haiku20240307" => Model::Claude3Haiku20240307,
        "claude-3-sonnet-20240229" | "Claude3Sonnet20240229" => Model::Claude3Sonnet20240229,
        "claude-3-opus-20240229" | "Claude3Opus20240229" => Model::Claude3Opus20240229,
        "claude-3-5-sonnet-20241022" | "Claude35Sonnet20241022" => Model::Claude35Sonnet20241022,
        "claude-3-5-sonnet-20250114" | "Claude35Sonnet20250114" => Model::Claude35Sonnet20250114,
        "claude-4-sonnet-20250514" | "Claude4Sonnet20250514" => Model::Claude4Sonnet20250514,
        _ => Model::Claude35Sonnet20241022,
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
        let c = payload_to_string(payload);
        if c.is_empty() {
            continue;
        }
        let role_enum = match role {
            ChatSessionRoleType::User => Role::User,
            ChatSessionRoleType::Ai => Role::Assistant,
        };
        v.push((role_enum, ContentBlock::text(c)));
    }
    v.push((Role::User, ContentBlock::text(user_text.to_string())));
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
    let model_enum = model_from_str(model);

    let client = Client::builder()
        .api_key(api_key)
        .model(model_enum)
        .max_tokens(8192)
        .build()
        .map_err(|e| format!("Claude client: {}", e))?;

    let mut builder = client.chat_builder().system(system_prompt);
    for (role, content) in build_messages(session_messages, user_text) {
        builder = builder.message(role, content);
    }
    let request = builder.tools(tools_list).build();

    let mut stream = client
        .stream_chat(request)
        .await
        .map_err(|e| format!("Claude stream: {}", e))?;

    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;
    let mut message_created = false;
    let mut tool_uses: Vec<(String, String, Value)> = Vec::new();

    tracing::info!(tool_count, "Claude: stream started with tools (anthropic_rust)");

    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| format!("Claude stream event: {}", e))?;
        match &event {
            StreamEvent::ContentBlockDelta { delta, .. } => {
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
            StreamEvent::ContentBlockStart { content_block, .. } => {
                if let ContentBlock::ToolUse { id, name, input } = content_block {
                    tool_uses.push((id.clone(), name.clone(), input.clone()));
                }
            }
            StreamEvent::MessageStop => break,
            _ => {}
        }
    }

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
