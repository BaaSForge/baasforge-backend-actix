use crate::configs::Settings;
use crate::database::{block_on_db_tokio, establish_pool};
use crate::models::chat_messages::ChatMessageStatus;
use crate::routes::chat::message_queue::{
    self as chat_message_queue,
    chat_dead_letter_spec, chat_retry_policy,
    parse_chat_routing_key, ChatSessionQueueMessage,
};
use crate::routes::ws::events::{ChatSessionMessageStatusChangePayload, WsServerEvent};
use crate::routes::ws::redis::{is_message_stopped, publish_event_to_channel};
use crate::schema::chat_messages;
use crate::services::chat_worker::llm::build_llm_client;
use crate::services::chat_worker::processor::process_one;
use crate::services::message_queue::{publish_to_dead_letter, publish_to_retry_delay};
use diesel::prelude::*;
use lapin::options::BasicAckOptions;
use lapin::ConnectionProperties;
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
    let llm_client = build_llm_client(&settings)?;
    info!(provider = %llm_provider, "Using LLM provider");

    let redis_client = redis::Client::open(settings.redis_url.as_str())?;
    let redis_conn = redis_client.get_multiplexed_async_connection().await?;
    let redis_conn = Arc::new(Mutex::new(redis_conn));
    info!("Connected to Redis for streaming chunks");

    let pool = establish_pool(
        settings.pg_database_url.clone(),
        settings.database_pool_size,
        settings.database_connection_timeout_secs,
    );
    let rabbit_url = settings.rabbit_url.clone();
    let retry_delay_exchange = settings.retry_delay_exchange.clone();

    info!("Connecting to RabbitMQ...");
    let connection =
        lapin::Connection::connect(&rabbit_url, ConnectionProperties::default()).await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    let (_consumer_channel, mut consumer) =
        chat_message_queue::create_chat_work_consumer_on_connection(&connection).await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    let retry_channel = connection.create_channel().await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
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

        // Placeholder is required; deserialization would have failed if missing (field is i64)
        let placeholder_ai_message_id = payload.placeholder_ai_message_id;

        // If user already requested stop for this message, ack and skip so it is not picked by another consumer
        {
            let mut guard = redis_conn.lock().await;
            if let Ok(true) = is_message_stopped(&mut *guard, placeholder_ai_message_id).await {
                tracing::info!(
                    project_id,
                    session_id,
                    placeholder_ai_message_id,
                    "Message marked stopped in Redis, acking and skipping"
                );
                let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                continue;
            }
        }

        let retry_num = payload.retry_num;
        let pool = pool.clone();
        let redis_conn = redis_conn.clone();
        let llm_client = llm_client.clone();
        let acker = delivery.acker.clone();
        let retry_channel = retry_channel.clone();
        let retry_delay_exchange = retry_delay_exchange.clone();
        let delivery_data = delivery.data.clone();
        let retry_routing_key = routing_key.clone();
        let retry_policy = chat_retry_policy();
        let dead_letter_spec = chat_dead_letter_spec();

        tokio::spawn(async move {
            let result = process_one(
                &pool,
                &llm_client,
                &redis_conn,
                user_id,
                project_id,
                session_id,
                &user_text,
                placeholder_ai_message_id,
            )
            .await;

            match &result {
                Ok(()) => {}
                Err(e) => {
                    if retry_policy.should_dead_letter(retry_num) {
                        tracing::error!(
                            project_id,
                            session_id,
                            retry_num,
                            "Process message failed after max retries, sending to dead letter: {}",
                            e
                        );
                        if let Err(dl_e) = publish_to_dead_letter(
                            &retry_channel,
                            &dead_letter_spec.exchange_name,
                            &retry_routing_key,
                            &delivery_data,
                        )
                        .await
                        {
                            tracing::error!("Failed to publish to dead letter: {}", dl_e);
                        }
                        let message_id_to_update = placeholder_ai_message_id;
                        // Update placeholder message status to failed so client and API reflect it
                        if let Err(db_e) = block_on_db_tokio(&pool, move |conn| {
                            diesel::update(
                                chat_messages::table
                                    .filter(chat_messages::id.eq(message_id_to_update)),
                            )
                            .set(chat_messages::status.eq(ChatMessageStatus::Failed))
                            .execute(conn)
                        })
                        .await
                        {
                            tracing::error!(
                                "Failed to update message status to failed: {}",
                                db_e
                            );
                        } else {
                            // Notify clients subscribed to this chat session so they can update UI
                            let topic =
                                format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
                            let event = WsServerEvent::ChatSessionMessageStatusChange(
                                ChatSessionMessageStatusChangePayload {
                                    message_id: placeholder_ai_message_id,
                                    status: ChatMessageStatus::Failed,
                                },
                            );
                            let mut guard = redis_conn.lock().await;
                            if let Err(redis_e) =
                                publish_event_to_channel(&mut *guard, &topic, &event).await
                            {
                                tracing::warn!(
                                    "Failed to publish message status change to Redis: {}",
                                    redis_e
                                );
                            }
                        }
                    } else {
                        tracing::error!(
                            project_id,
                            session_id,
                            retry_num,
                            "Process message failed, sending to retry delay: {}",
                            e
                        );
                        let next_retry_num = retry_num + 1;
                        let mut payload_for_retry: ChatSessionQueueMessage =
                            match serde_json::from_slice(&delivery_data) {
                                Ok(p) => p,
                                Err(parse_e) => {
                                    tracing::error!("Failed to parse message for retry: {}", parse_e);
                                    if let Err(dl_e) = publish_to_dead_letter(
                                        &retry_channel,
                                        &dead_letter_spec.exchange_name,
                                        &retry_routing_key,
                                        &delivery_data,
                                    )
                                    .await
                                    {
                                        tracing::error!("Failed to publish to dead letter: {}", dl_e);
                                    }
                                    if acker.ack(BasicAckOptions::default()).await.is_err() {
                                        tracing::warn!("Failed to ack delivery");
                                    }
                                    return;
                                }
                            };
                        payload_for_retry.retry_num = next_retry_num;
                        let payload_bytes = match serde_json::to_vec(&payload_for_retry) {
                            Ok(b) => b,
                            Err(ser_e) => {
                                tracing::error!("Failed to serialize message for retry: {}", ser_e);
                                if acker.ack(BasicAckOptions::default()).await.is_err() {
                                    tracing::warn!("Failed to ack delivery");
                                }
                                return;
                            }
                        };
                        if let Err(retry_e) = publish_to_retry_delay(
                            &retry_channel,
                            &retry_delay_exchange,
                            &retry_routing_key,
                            &payload_bytes,
                            retry_num,
                            &retry_policy,
                        )
                        .await
                        {
                            tracing::error!("Failed to publish to retry delay: {}", retry_e);
                        }
                    }
                }
            }

            if acker.ack(BasicAckOptions::default()).await.is_err() {
                tracing::warn!("Failed to ack delivery");
            }
        });
    }

    Ok(())
}

