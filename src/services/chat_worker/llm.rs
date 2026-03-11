use crate::models::chat_messages::ChatSessionRoleType;
use crate::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use crate::routes::ws::publish_event_to_channel;
use gemini_rust::client::GenerationStream;
use gemini_rust::Gemini;
use openai::chat::{ChatCompletionBuilder, ChatCompletionMessage, ChatCompletionMessageRole};
use openai::Credentials;
use redis::aio::MultiplexedConnection;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc::Receiver, Mutex};

fn payload_to_string(payload: &Option<Value>) -> String {
    match payload {
        Some(v) => v.as_str().map(String::from).unwrap_or_else(|| v.to_string()),
        None => String::new(),
    }
}

fn build_chat_messages(
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
) -> Vec<ChatCompletionMessage> {
    let mut messages: Vec<ChatCompletionMessage> = vec![ChatCompletionMessage {
        role: ChatCompletionMessageRole::System,
        content: Some("You are a helpful assistant.".to_string()),
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
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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

pub async fn call_gpt_stream(
    credentials: &Credentials,
    model: &str,
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let messages = build_chat_messages(session_messages, user_text);

    let mut rx: Receiver<openai::chat::ChatCompletionDelta> = ChatCompletionBuilder::default()
        .model(model)
        .messages(messages)
        .credentials(credentials.clone())
        .create_stream()
        .await
        .map_err(|e| format!("OpenAI stream: {}", e))?;

    let mut full_text = String::new();
    let mut chunk_index: u32 = 0;

    while let Some(delta) = rx.recv().await {
        for choice in &delta.choices {
            if let Some(ref content) = choice.delta.content {
                if !content.is_empty() {
                    full_text.push_str(content);
                    if let Some(ref conn) = redis_conn {
                        let payload = ChatSessionNewMessageChunkPayload::new(
                            topic.to_string(),
                            chunk_index,
                            content.clone(),
                            false,
                        );
                        let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
                        let mut guard = conn.lock().await;
                        if let Err(e) =
                            publish_event_to_channel(&mut *guard, topic, &event).await
                        {
                            tracing::warn!("Failed to publish chat chunk to Redis: {}", e);
                        }
                        chunk_index += 1;
                    }
                }
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

