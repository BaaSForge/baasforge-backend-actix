// TODO: we know that gpt and gemini dont support tool call and content in the same response,
// so we can simplify this code
use crate::models::chat_messages::ChatSessionRoleType;
use crate::services::chat_worker::agent::BASE_SYSTEM_PROMPT;
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::services::chat_worker::llm::GenerateOutcome;
use gemini_rust::client::GenerationStream;
use gemini_rust::tools::model::{FunctionCallingMode, FunctionDeclaration};
use gemini_rust::Gemini;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::{payload_to_string, tools, BoxError};
use serde_json::Value;

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
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
    message_id: i64,
    tool_context: Option<&tools::ToolContext>,
) -> Result<GenerateOutcome, BoxError> {
    let client = Gemini::with_model(api_key.to_string(), gemini_model_id(model))
        .map_err(|e| format!("Gemini client: {}", e))?;

    let declarations: Vec<FunctionDeclaration> = tools::to_gemini_declarations();
    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;

    let mut request = client
        .generate_content()
        .with_system_prompt(BASE_SYSTEM_PROMPT);

    for (role, payload) in session_messages {
        let text = payload_to_string(payload);
        if text.is_empty() {
            continue;
        }
        request = match role {
            ChatSessionRoleType::User => request.with_user_message(text),
            ChatSessionRoleType::Ai => request.with_model_message(text),
        };
    }

    request = request
        .with_user_message(user_text.to_string())
        .with_function_calling_mode(FunctionCallingMode::Auto);
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

    let mut all_function_calls: Vec<(String, Value)> = Vec::new();
    let mut chunk_count: u32 = 0;

    while let Some(chunk) = futures::TryStreamExt::try_next(&mut stream)
        .await
        .map_err(|e| format!("Gemini stream: {}", e))?
    {
        chunk_count += 1;
        for fc in chunk.function_calls() {
            all_function_calls.push((fc.name.clone(), fc.args.clone()));
            tracing::info!(
                chunk_index = chunk_count,
                tool = %fc.name,
                "Gemini: found function call in stream chunk"
            );
        }
        let text: String = chunk.text().to_string();
        if !text.is_empty() {
            full_text.push_str(&text);
            if let Some(ref conn) = redis_conn {
                let mut guard = conn.lock().await;
                if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                    return Ok(GenerateOutcome::Stopped(full_text));
                }
                let payload = ChatSessionNewMessageChunkPayload::new(
                    topic.to_string(),
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
    if !all_function_calls.is_empty() {
        tracing::info!(
            "Gemini: LLM requested {} tool call(s), executing",
            fc_count
        );
        for (name, args) in all_function_calls {
            tracing::info!(tool = %name, args = ?args, "Gemini: executing tool");
            let _result = tools::execute_tool(&name, &args, tool_context)
                .map_err(|e| format!("Tool execution failed: {}", e))?;
        }
        // Tools executed; caller can re-invoke the LLM later with tool results in context
    } else {
        tracing::info!("Gemini: no tool calls in any stream chunk (function_call_count=0)");
    }

    if let Some(ref conn) = redis_conn {
        let payload = ChatSessionNewMessageChunkPayload::new(
            topic.to_string(),
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

    Ok(GenerateOutcome::Completed(full_text))
}
