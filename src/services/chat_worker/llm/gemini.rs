use crate::models::chat_messages::ChatSessionRoleType;
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::publish_event_to_channel;
use gemini_rust::client::GenerationStream;
use gemini_rust::Gemini;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::{payload_to_string, BoxError};
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
) -> Result<String, BoxError> {
    let client = Gemini::with_model(api_key.to_string(), gemini_model_id(model))
        .map_err(|e| format!("Gemini client: {}", e))?;

    let mut request = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant.");

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

    let mut stream: GenerationStream = request
        .with_user_message(user_text.to_string())
        .execute_stream()
        .await
        .map_err(|e| format!("Gemini stream: {}", e))?;

    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;

    while let Some(chunk) = futures::TryStreamExt::try_next(&mut stream)
        .await
        .map_err(|e| format!("Gemini stream: {}", e))?
    {
        let text: String = chunk.text().to_string();
        if !text.is_empty() {
            full_text.push_str(&text);
            if let Some(ref conn) = redis_conn {
                let payload = ChatSessionNewMessageChunkPayload::new(
                    topic.to_string(),
                    chunk_index,
                    text,
                    false,
                );
                let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
                let mut guard = conn.lock().await;
                if let Err(e) = publish_event_to_channel(&mut *guard, topic, &event).await {
                    tracing::warn!("Failed to publish chat chunk to Redis: {}", e);
                }
                chunk_index += 1;
            }
        }
    }

    if let Some(ref conn) = redis_conn {
        let payload = ChatSessionNewMessageChunkPayload::new(
            topic.to_string(),
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

    Ok(full_text)
}

