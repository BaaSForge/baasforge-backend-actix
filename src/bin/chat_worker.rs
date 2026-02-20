use baal_backend_actix::configs::Settings;
use baal_backend_actix::database::{establish_pool, DbPool};
use baal_backend_actix::models::chat_messages::{
    ChatMessageInsert, ChatMessageRow, ChatMessageType, ChatSessionRoleType,
};
use baal_backend_actix::routes::chat::message_queue::{
    self as chat_message_queue, parse_chat_routing_key, ChatSessionQueueMessage,
};
use baal_backend_actix::routes::ws::events::ChatSessionNewMessageChunkPayload;
use baal_backend_actix::routes::ws::publish_chat_chunk;
use baal_backend_actix::schema::{chat_messages};
use diesel::prelude::*;
use lapin::options::BasicAckOptions;
use openai::chat::{ChatCompletionBuilder, ChatCompletionMessage, ChatCompletionMessageRole};
use openai::Credentials;
use serde_json::Value;
use tokio_stream::StreamExt;
use tracing::info;

fn load_previous_session_messages(
    pool: &DbPool,
    session_id: i64,
    limit: i64,
) -> Result<Vec<(ChatSessionRoleType, Option<Value>)>, diesel::result::Error> {
    let mut conn = pool.get().map_err(|e| diesel::result::Error::QueryBuilderError(Box::new(e)))?;
    let rows = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .order(chat_messages::id.asc())
        .limit(limit)
        .select(ChatMessageRow::as_select())
        .load::<ChatMessageRow>(&mut conn)?;

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

async fn call_gpt(
    credentials: &Credentials,
    session_messages: &[(ChatSessionRoleType, Option<Value>)],
    user_text: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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

    let request = ChatCompletionBuilder::default()
        .model("gpt-4o-mini")
        .messages(messages)
        .credentials(credentials.clone())
        .build()
        .map_err(|e| format!("build chat request: {}", e))?;

    let completion = openai::chat::ChatCompletion::create(request)
        .await
        .map_err(|e| format!("OpenAI API: {}", e))?;

    let text = completion
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();
    Ok(text)
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

    let pool = establish_pool(settings.pg_database_url.clone());
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
        let settings = settings.clone();

        let acker = delivery.acker.clone();
        tokio::spawn(async move {
            let result = process_one(
                &pool,
                &credentials,
                &settings,
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
    settings: &Settings,
    user_id: i64,
    project_id: i64,
    session_id: i64,
    user_text: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session_messages = load_previous_session_messages(pool, session_id, 50)?;
    let ai_text = call_gpt(credentials, &session_messages, user_text).await?;

    let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
    if let Some(ref redis_url) = settings.redis_url {
        let payload = ChatSessionNewMessageChunkPayload::new(
            topic.clone(),
            0,
            ai_text.clone(),
            true,
        );
        if let Err(e) = publish_chat_chunk(redis_url, &topic, payload).await {
            tracing::warn!(%project_id, %session_id, "Failed to publish chat chunk to Redis: {}", e);
        }
    }

    let mut conn = pool.get().map_err(|e| format!("pool: {}", e))?;
    let insert = ChatMessageInsert {
        chat_session_id: session_id,
        role: ChatSessionRoleType::Ai,
        type_: ChatMessageType::Text,
        payload: Some(serde_json::to_value(ai_text.clone())?),
    };
    diesel::insert_into(chat_messages::table)
        .values(&insert)
        .execute(&mut conn)?;

    info!(project_id, session_id, "Processed and saved AI response");
    Ok(())
}
