use crate::models::chat_messages::{
    ChatAgentType, ChatMessageType, ChatSessionRoleType, RequestedToolCall, TextPayload,
};
use crate::services::chat_worker::llm::history::{
    session_messages_to_upstream_entries, UpstreamHistoryEntry,
};
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::services::chat_worker::agent::system_prompt_for_agent;
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::llm::GenerateOutcome;
use gemini_rust::client::GenerationStream;
use gemini_rust::tools::model::{FunctionCall, FunctionCallingMode, FunctionDeclaration};
use gemini_rust::{Content, Gemini, Message, Role};
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::tools::{self, load_latest_production_release_units, load_latest_releases};
use super::BoxError;
use crate::services::chat_worker::agent::tool_names_for_agent;
use serde_json::Value;

fn gemini_declaration_fns_by_name() -> &'static [(
    &'static str,
    fn() -> gemini_rust::tools::model::FunctionDeclaration,
)] {
    &[
        (
            load_latest_production_release_units::TOOL_NAME,
            load_latest_production_release_units::gemini_declaration,
        ),
        (
            load_latest_releases::TOOL_NAME,
            load_latest_releases::gemini_declaration,
        ),
    ]
}

/// Gemini declarations for the given agent only (subset of all tools).
fn to_gemini_declarations_for_agent(
    agent: &ChatAgentType,
) -> Vec<gemini_rust::tools::model::FunctionDeclaration> {
    let names = tool_names_for_agent(agent);
    gemini_declaration_fns_by_name()
        .iter()
        .filter(|(name, _)| names.contains(name))
        .map(|(_, f)| f())
        .collect()
}

fn gemini_model_id(model: &str) -> String {
    if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{}", model)
    }
}

pub async fn call_gemini_stream(
    api_key: &str,
    model: &str,
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
) -> Result<GenerateOutcome, BoxError> {
    let client = Gemini::with_model(api_key, gemini_model_id(model))
        .map_err(|e| format!("Gemini client: {}", e))?;

    let system_prompt = system_prompt_for_agent(&agent);
    let declarations: Vec<FunctionDeclaration> = to_gemini_declarations_for_agent(&agent);
    let mut full_text = String::new();
    // Incremented only when we publish a non-final assistant text chunk to Redis.
    let mut chunk_index: u32 = 0;

    let entries = session_messages_to_upstream_entries(session_messages, user_text);
    let mut request = client.generate_content().with_system_prompt(system_prompt);

    for entry in &entries {
        match entry {
            UpstreamHistoryEntry::User { text } => {
                if !text.is_empty() {
                    request = request.with_user_message(text.clone());
                }
            }
            UpstreamHistoryEntry::Assistant { text, tool_calls, .. } => {
                if let Some(tcs) = tool_calls.as_ref() {
                    if !text.trim().is_empty() {
                        request = request.with_model_message(text.trim().to_string());
                    }
                    for tc in tcs.iter() {
                        if tc.name.is_empty() {
                            continue;
                        }
                        let Some(sig) = &tc.thought_signature else {
                            continue;
                        };
                        let model_content = Content::function_call_with_thought(
                            FunctionCall::new(tc.name.clone(), tc.arguments.clone()),
                            sig.clone(),
                        );
                        request = request.with_message(Message {
                            content: model_content,
                            role: Role::Model,
                        });
                    }
                } else if !text.trim().is_empty() {
                    request = request.with_model_message(text.trim().to_string());
                }
            }
            UpstreamHistoryEntry::ToolResult {
                tool_name,
                content,
                ..
            } => {
                request = request
                    .with_function_response_str(tool_name, content.clone())
                    .map_err(|e| format!("Gemini history function response: {}", e))?;
            }
        }
    }

    request = request.with_function_calling_mode(FunctionCallingMode::Auto);
    for decl in &declarations {
        request = request.with_function(decl.clone());
    }

    tracing::info!(
        tool_count = declarations.len(),
        "Gemini: stream started with tools attached"
    );

    let mut stream: GenerationStream = request
        .execute_stream()
        .await
        .map_err(|e| format!("Gemini stream: {}", e))?;

    let mut all_function_calls: Vec<(String, Value, Option<String>)> = Vec::new();
    let mut chunk_count: u32 = 0;

    while let Some(chunk) = futures::TryStreamExt::try_next(&mut stream)
        .await
        .map_err(|e| format!("Gemini stream: {}", e))?
    {
        chunk_count += 1;
        for (fc, thought_sig) in chunk.function_calls_with_thoughts() {
            all_function_calls.push((
                fc.name.clone(),
                fc.args.clone(),
                thought_sig.map(String::from),
            ));
            tracing::info!(
                chunk_index = chunk_count,
                tool = %fc.name,
                has_thought_signature = thought_sig.is_some(),
                "Gemini: found function call in stream chunk"
            );
        }
        let text: String = chunk.text().to_string();
        if !text.is_empty() {
            full_text.push_str(&text);
            if let Some(ref conn) = redis_conn {
                let mut guard = conn.lock().await;
                if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                    return Ok(GenerateOutcome::Stopped(TextPayload {
                        text: full_text,
                        requested_tool_calls: None,
                        ..Default::default()
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
                let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
                if let Err(e) = publish_event_to_channel(&mut *guard, topic, &event).await {
                    tracing::warn!("Failed to publish chat chunk to Redis: {}", e);
                }
                chunk_index += 1;
            }
        }
    }

    let fc_count = all_function_calls.len();
    tracing::info!(
        stream_chunks = chunk_count,
        function_call_count = fc_count,
        "Gemini: stream ended, checking for tool calls"
    );
    let requested_tool_calls = if all_function_calls.is_empty() {
        None
    } else {
        tracing::info!("Gemini: LLM requested {} tool call(s), executing", fc_count);
        let requested: Vec<RequestedToolCall> = all_function_calls
            .iter()
            .map(|(name, args, thought_sig)| RequestedToolCall {
                tool_call_id: None, // Gemini does not provide tool call ids in stream
                tool_name: name.clone(),
                tool_arguments_to_pass: args.clone(),
                thought_signature: thought_sig.clone(),
            })
            .collect();

        // No streamed assistant text was published to Redis; persist tool-call payload for the client.
        if chunk_index == 0 {
            let invoker_payload = TextPayload {
                text: full_text.clone(),
                requested_tool_calls: Some(requested.clone()),
                ..Default::default()
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
                .map_err(|e| format!("ai_message: {}", e))?;
            }
        }

        for (name, args, _thought_sig) in &all_function_calls {
            tracing::info!(tool = %name, args = ?args, "Gemini: executing tool");
            tools::execute_tool_async(name, args.clone(), None)
                .await
                .map_err(|e| format!("Tool execution failed: {}", e))?;
        }
        Some(requested)
    };

    if all_function_calls.is_empty() {
        tracing::info!("Gemini: no tool calls in any stream chunk (function_call_count=0)");
    }

    if chunk_index > 0 && let Some(ref conn) = redis_conn {
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
        ..Default::default()
    }))
}
