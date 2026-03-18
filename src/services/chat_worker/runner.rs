use crate::configs::Settings;
use crate::database::{block_on_db_tokio, establish_pool};
use crate::models::chat_messages::{
    ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
};
use crate::routes::chat::message_queue::{
    self as chat_message_queue,
    chat_dead_letter_spec, chat_retry_policy,
    parse_chat_routing_key, ChatSessionQueueMessage,
};
use crate::routes::ws::events::{ChatSessionMessageCreatedPayload, ChatSessionMessageStatusChangePayload, WsServerEvent};
use crate::routes::ws::redis::{is_message_stopped, publish_event_to_channel};
use crate::routes::ws::user_channel;
use crate::services::chat_worker::generation_lifecycle;
use crate::schema::chat_messages;
use crate::services::chat_worker::llm::{build_llm_client, GenerateOutcome};
use crate::services::chat_worker::llm::tools::{spawn_tool_message_handler, ToolMessageContext};
use crate::services::chat_worker::processor::process_one;
use crate::services::message_queue::{
    create_shared_publisher, publish_to_dead_letter, publish_to_retry_delay,
};
use diesel::prelude::*;
use lapin::options::BasicAckOptions;
use lapin::ConnectionProperties;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::info;
use uuid::Uuid;

use crate::database::DbPool;

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
    let rabbit_publisher = create_shared_publisher(
        &rabbit_url,
        chat_message_queue::CHAT_SESSION_EXCHANGE,
    )
    .await
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

        let response_message_id = payload.response_message_id;

        if payload.type_ == ChatMessageType::Text {
            let (user_id, user_text) = match payload.as_text_message_for_processing() {
                Some(x) => x,
                None => {
                    let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                    continue;
                }
            };
            let agent = payload.agent.clone();

            {
                let mut guard = redis_conn.lock().await;
                if let Ok(true) = is_message_stopped(&mut *guard, response_message_id).await {
                    tracing::info!(
                        %project_id,
                        %session_id,
                        %response_message_id,
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
            let rabbit_publisher = rabbit_publisher.clone();
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
                    &rabbit_publisher,
                    user_id,
                    project_id,
                    session_id,
                    &user_text,
                    response_message_id,
                    agent,
                )
                .await;

            match &result {
                Ok(outcome) => {
                    // Continuation is scheduled only when LLM returned tool calls (tools will eventually call maybe_complete_invoking_message and publish).
                    let will_schedule_continuation = matches!(
                        outcome,
                        GenerateOutcome::Completed(p)
                            if p.requested_tool_calls.as_ref().map_or(false, |c| !c.is_empty())
                    );
                    generation_lifecycle::notify_if_generation_loop_finished(
                        &redis_conn,
                        user_id,
                        project_id,
                        session_id,
                        will_schedule_continuation,
                    )
                    .await;
                }
                Err(e) => {
                    if retry_policy.should_dead_letter(retry_num) {
                        tracing::error!(
                            %project_id,
                            %session_id,
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
                        let message_id_to_update = response_message_id;
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
                            // Notify clients subscribed to this user so they can update UI
                            let channel = user_channel(user_id);
                            let event = WsServerEvent::ChatSessionMessageStatusChange(
                                ChatSessionMessageStatusChangePayload {
                                    project_id,
                                    session_id,
                                    message_id: response_message_id,
                                    status: ChatMessageStatus::Failed,
                                },
                            );
                            let mut guard = redis_conn.lock().await;
                            if let Err(redis_e) =
                                publish_event_to_channel(&mut *guard, &channel, &event).await
                            {
                                tracing::warn!(
                                    "Failed to publish message status change to Redis: {}",
                                    redis_e
                                );
                            }
                        }
                    } else {
                        tracing::error!(
                            %project_id,
                            %session_id,
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
        } else {
            // Non-TEXT: create message in DB, publish Redis "new message created", then type-specific handler.
            let user_id = payload.user_id.expect("validated");
            let payload_json = payload
                .payload
                .as_ref()
                .and_then(|p| serde_json::to_value(p).ok());
            let payload_json_for_insert = payload_json.clone();
            let role = payload.role.clone();
            let type_ = payload.type_.clone();
            let insert_message_id = response_message_id;
            let invoking_message_id_insert = payload.invoking_message_id;
            let agent_insert = payload.agent.clone();
            let pool_clone = pool.clone();
            let session_id_insert = session_id;
            let inserted_row = match block_on_db_tokio(&pool_clone, move |conn| {
                diesel::insert_into(chat_messages::table)
                    .values((
                        chat_messages::id.eq(insert_message_id),
                        chat_messages::chat_session_id.eq(session_id_insert),
                        chat_messages::role.eq(role),
                        chat_messages::type_.eq(type_),
                        chat_messages::payload.eq(payload_json_for_insert),
                        chat_messages::status.eq(ChatMessageStatus::Generating),
                        chat_messages::invoking_message_id.eq(invoking_message_id_insert),
                        chat_messages::agent.eq(Some(agent_insert)),
                    ))
                    .returning(ChatMessageRow::as_returning())
                    .get_result(conn)
            })
            .await
            {
                Ok(row) => row,
                Err(e) => {
                    tracing::error!(
                        %project_id,
                        %session_id,
                        %response_message_id,
                        "Failed to create non-TEXT message in DB: {}",
                        e
                    );
                    let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                    continue;
                }
            };
            let channel = user_channel(user_id);
            let created_message = ChatMessageResponse::from(inserted_row);
            let event = WsServerEvent::ChatSessionMessageCreated(ChatSessionMessageCreatedPayload {
                project_id,
                session_id,
                message: created_message,
            });
            {
                let mut guard = redis_conn.lock().await;
                if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
                    tracing::warn!("Failed to publish new message to Redis: {}", e);
                }
            }
            
            spawn_tool_message_handler(
                payload.type_.clone(),
                ToolMessageContext {
                    pool: pool.clone(),
                    redis_conn: redis_conn.clone(),
                    project_id,
                    session_id,
                    message_id: response_message_id,
                    user_id,
                    payload: payload_json,
                    invoking_message_id: payload.invoking_message_id,
                    rabbit_publisher: Some(rabbit_publisher.clone()),
                },
            );
            let _ = delivery.acker.ack(BasicAckOptions::default()).await;
        }
    }

    Ok(())
}

