use crate::configs::{LlmProvider, Settings};
use crate::database::establish_pool;
use crate::routes::chat::message_queue::{
    self as chat_message_queue, parse_chat_routing_key, ChatSessionQueueMessage,
};
use crate::services::chat_worker::processor::process_one;
use lapin::options::BasicAckOptions;
use openai::Credentials;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::info;

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    let settings =
        Settings::new().map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            e.to_string().into()
        })?;
    let llm_provider = settings.llm_provider;
    let (openai_credentials, openai_model, gemini_api_key, gemini_model) = match llm_provider {
        LlmProvider::OpenAi => {
            let key = settings.openai_api_key.as_deref().ok_or_else(|| {
                Box::<dyn std::error::Error + Send + Sync>::from(
                    "LLM_PROVIDER=openai but OPENAI_API_KEY (or OPENAI_KEY) is not set",
                )
            })?;
            (
                Some(Credentials::new(key, "")),
                settings.openai_model.clone(),
                None,
                String::new(),
            )
        }
        LlmProvider::Gemini => {
            let key = settings.gemini_api_key.clone().ok_or_else(|| {
                Box::<dyn std::error::Error + Send + Sync>::from(
                    "LLM_PROVIDER=gemini but GEMINI_API_KEY is not set",
                )
            })?;
            (
                None,
                String::new(),
                Some(key),
                settings.gemini_model.clone(),
            )
        }
    };
    info!(provider = %llm_provider, "Using LLM provider");

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
        let redis_conn = redis_conn.clone();
        let openai_credentials = openai_credentials.clone();
        let openai_model = openai_model.clone();
        let gemini_api_key = gemini_api_key.clone();
        let gemini_model = gemini_model.clone();

        let acker: lapin::acker::Acker = delivery.acker.clone();
        tokio::spawn(async move {
            let result = process_one(
                &pool,
                llm_provider,
                openai_credentials.as_ref(),
                &openai_model,
                gemini_api_key.as_deref(),
                &gemini_model,
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

