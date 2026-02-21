use baal_backend_actix::configs::Settings;
use baal_backend_actix::database::{block_on_db_tokio, establish_pool, DbPool, PooledConn};
use baal_backend_actix::models::chat_messages::{
    ChatMessageInsert, ChatMessageRow, ChatMessageType, ChatSessionRoleType,
};
use baal_backend_actix::routes::chat::message_queue::{
    self as chat_message_queue, parse_chat_routing_key, ChatSessionQueueMessage,
};
use baal_backend_actix::routes::ws::events::{ChatSessionNewMessageChunkPayload, WsServerEvent};
use baal_backend_actix::routes::ws::publish_event_to_channel;
use baal_backend_actix::schema::{chat_messages};
use diesel::prelude::*;
use lapin::options::BasicAckOptions;
use openai::chat::{ChatCompletionBuilder, ChatCompletionMessage, ChatCompletionMessageRole};
use openai::Credentials;
use redis::aio::MultiplexedConnection;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{mpsc::Receiver, Mutex};
use tokio_stream::StreamExt;
use tracing::info;

fn load_previous_session_messages(
    conn: &mut PooledConn,
    session_id: i64,
    limit: i64,
) -> Result<Vec<(ChatSessionRoleType, Option<Value>)>, diesel::result::Error> {
    let rows = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .order(chat_messages::id.asc())
        .limit(limit)
        .select(ChatMessageRow::as_select())
        .load::<ChatMessageRow>(conn)?;

    let out = rows
        .into_iter()
        .map(|row| (row.role, row.payload))
        .collect();
    Ok(out)
}

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
    let mut messages: Vec<ChatCompletionMessage> = vec![
        ChatCompletionMessage {
            role: ChatCompletionMessageRole::System,
            content: Some("You are a helpful assistant.".to_string()),
            name: None,
            function_call: None,
            tool_call_id: None,
            tool_calls: None,
        },
    ];

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

async fn call_gpt_stream(
    credentials: &Credentials,
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let messages = build_chat_messages(session_messages, user_text);

    let mut rx: Receiver<openai::chat::ChatCompletionDelta> = ChatCompletionBuilder::default()
        .model("gpt-4o-mini")
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    let settings = Settings::new().map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
        e.to_string().into()
    })?;
    let openai_key = settings.openai_api_key.as_deref().ok_or_else(|| {
        Box::<dyn std::error::Error + Send + Sync>::from(
            "OPENAI_API_KEY or OPENAI_KEY must be set for the chat worker",
        )
    })?;
    let credentials = Credentials::new(openai_key, "");

    let redis_url = settings.redis_url.as_deref().ok_or_else(|| {
        Box::<dyn std::error::Error + Send + Sync>::from(
            "REDIS_URL must be set for the chat worker (required for streaming chunks to WebSocket clients)",
        )
    })?;
    let redis_client = redis::Client::open(redis_url)?;
    let redis_conn = redis_client.get_multiplexed_async_connection().await?;
    let redis_conn = Arc::new(Mutex::new(redis_conn));
    info!("Connected to Redis for streaming chunks");

    let pool = establish_pool(
        settings.pg_database_url.clone(),
        settings.database_pool_size,
        settings.database_connection_timeout_secs,
    );
    let rabbit_url = settings.rabbit_url.clone();

    info!("Connecting to RabbitMQ and creating work consumer...");
    let (_connection, mut consumer): (lapin::Connection, lapin::Consumer) =
        chat_message_queue::create_chat_work_consumer(&rabbit_url).await?;
    info!("Chat worker started. Consuming from work queue.");

    while let Some(delivery_result) = consumer.next().await {
        let delivery = match delivery_result {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Consumer error: {}", e);
                continue;
            }
        };

        let routing_key = delivery.routing_key.to_string();
        let (project_id, session_id) = match parse_chat_routing_key(&routing_key) {
            Some(ids) => ids,
            None => {
                tracing::warn!("Invalid routing key, acking and skipping: {}", routing_key);
                let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                continue;
            }
        };

        let payload = match serde_json::from_slice::<ChatSessionQueueMessage>(&delivery.data) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Invalid message body: {}", e);
                let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                continue;
            }
        };

        if let Err(e) = payload.validate() {
            tracing::error!("Invalid queue message: {}", e);
            let _ = delivery.acker.ack(BasicAckOptions::default()).await;
            continue;
        }

        let (user_id, user_text) = match payload.as_user_text_message() {
            Some(x) => x,
            None => {
                let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                continue;
            }
        };

        let pool = pool.clone();
        let credentials = credentials.clone();
        let redis_conn = redis_conn.clone();

        let acker: lapin::acker::Acker = delivery.acker.clone();
        tokio::spawn(async move {
            let result = process_one(
                &pool,
                &credentials,
                &redis_conn,
                user_id,
                project_id,
                session_id,
                &user_text,
            )
            .await;

            if let Err(e) = &result {
                tracing::error!(
                    project_id,
                    session_id,
                    "Process message failed: {}",
                    e
                );
            }

            if acker.ack(BasicAckOptions::default()).await.is_err() {
                tracing::warn!("Failed to ack delivery");
            }
        });
    }

    Ok(())
}

async fn process_one(
    pool: &DbPool,
    credentials: &Credentials,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    user_id: i64,
    project_id: i64,
    session_id: i64,
    user_text: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session_messages = block_on_db_tokio(pool, |conn| load_previous_session_messages(conn, session_id, 50))
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
    let ai_text = call_gpt_stream(
        credentials,
        &session_messages,
        user_text,
        Some(redis_conn.clone()),
        &topic,
    )
    .await?;

    let insert = ChatMessageInsert {
        chat_session_id: session_id,
        role: ChatSessionRoleType::Ai,
        type_: ChatMessageType::Text,
        payload: Some(serde_json::to_value(ai_text.clone())?),
    };
    block_on_db_tokio(pool, move |conn| {
        diesel::insert_into(chat_messages::table)
            .values(&insert)
            .execute(conn)
    })
    .await
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    info!(project_id, session_id, "Processed and saved AI response");
    Ok(())
}
