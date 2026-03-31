use crate::configs::Settings;
use crate::database::{block_on_db_tokio, establish_pool};
use crate::libs::ids::new_uuid;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse,
    ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType, TextPayload,
};
use crate::routes::chat::message_queue::{
    self as chat_message_queue, ChatSessionQueueMessage, chat_dead_letter_spec, chat_retry_policy,
    parse_chat_routing_key, MAX_SUB_AGENT_STUCK_CONTINUATIONS,
};
use crate::routes::ws::events::{
    ChatSessionMessageCreatedPayload, ChatSessionMessageStatusChangePayload, WsServerEvent,
};
use crate::routes::ws::redis::{is_message_stopped, publish_event_to_channel};
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::generation_lifecycle;
use crate::services::chat_worker::llm::tools::{ToolMessageContext, spawn_tool_message_handler};
use crate::services::chat_worker::llm::{GenerateOutcome, build_llm_client};
use crate::services::chat_worker::processor::process_one;
use crate::services::chat_worker::release_controller_box;
use crate::services::message_queue::{
    create_shared_publisher, publish_to_dead_letter, publish_to_retry_delay,
};
use diesel::prelude::*;
use lapin::ConnectionProperties;
use lapin::options::BasicAckOptions;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::info;
use uuid::Uuid;

use crate::database::DbPool;

/// One-turn user line when a nested sub-agent completed with text but no `tool_calls` (see chat-queue flow).
const SUB_AGENT_STUCK_NUDGE: &str = "Your last reply in this nested run had no tool_calls. The worker only schedules your next turn after tool_calls execute (or after you call the finish tool). Reply again with at least one tool_call—use your data tools as needed, or call the finish tool when done. Do not send assistant text without tool_calls.";
use redis::aio::MultiplexedConnection;

/// Ensures the placeholder AI message exists with status Failed in the DB (inserts if it was never
/// created because the LLM failed before streaming any chunk), then notifies the client via
/// WebSocket. Call this whenever generation has failed and we will not retry.
async fn set_message_failed_and_notify(
    pool: &DbPool,
    redis_conn: &Arc<Mutex<MultiplexedConnection>>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    response_message_id: Uuid,
    agent: ChatAgentType,
) {
    let message_id = response_message_id;
    let session_id_db = session_id;
    let agent_db = agent;
    let db_ok = block_on_db_tokio(pool, move |conn| {
        // Update if row exists; otherwise insert (LLM may have failed before first chunk, so no row yet).
        diesel::insert_into(chat_messages::table)
            .values((
                chat_messages::id.eq(message_id),
                chat_messages::chat_session_id.eq(session_id_db),
                chat_messages::role.eq(ChatSessionRoleType::Ai),
                chat_messages::type_.eq(ChatMessageType::Text),
                chat_messages::payload.eq(None::<serde_json::Value>),
                chat_messages::status.eq(ChatMessageStatus::Failed),
                chat_messages::agent.eq(Some(agent_db)),
            ))
            .on_conflict(chat_messages::id)
            .do_update()
            .set(chat_messages::status.eq(ChatMessageStatus::Failed))
            .execute(conn)
    })
    .await
    .is_ok();

    if db_ok {
        generation_lifecycle::notify_if_generation_loop_finished(
            redis_conn, user_id, project_id, session_id, false,
        )
        .await;
        let channel = user_channel(user_id);
        let event =
            WsServerEvent::ChatSessionMessageStatusChange(ChatSessionMessageStatusChangePayload {
                project_id,
                session_id,
                message_id: response_message_id,
                status: ChatMessageStatus::Failed,
            });
        let mut guard = redis_conn.lock().await;
        if let Err(redis_e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
            tracing::warn!(
                "Failed to publish message status change to Redis: {}",
                redis_e
            );
        }
    } else {
        tracing::error!(
            %response_message_id,
            "Failed to upsert message status to Failed in DB"
        );
    }
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    let settings = Settings::new()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })?;
    info!(
        timeout_secs = settings.chat_consumer_timeout_secs,
        "CHAT_CONSUMER_TIMEOUT_SECS from env — `x-consumer-timeout` must cover one queue message (one LLM pass); see docs/chat-queue.md. Re-run queue_setup_http.sh after changing."
    );
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
    let connection = lapin::Connection::connect(&rabbit_url, ConnectionProperties::default())
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    let (_consumer_channel, mut consumer) =
        chat_message_queue::create_chat_work_consumer_on_connection(&connection)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    let retry_channel = connection
        .create_channel()
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    let rabbit_publisher =
        create_shared_publisher(&rabbit_url, chat_message_queue::CHAT_SESSION_EXCHANGE)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    info!("Chat worker started. Consuming from work queue.");

    'consumer: while let Some(delivery_result) = consumer.next().await {
        let delivery = match delivery_result {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Consumer error: {}", e);
                // Channel is dead (e.g. consumer ack timeout); exit so the supervisor can restart with a fresh connection.
                return Err(std::io::Error::other(format!("consumer error: {e}")).into());
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

        let response_message_id = payload.response_message_id;

        if payload.type_ == ChatMessageType::Text {
            let (user_id, user_text) = match payload.as_text_message_for_processing() {
                Some(x) => x,
                None => {
                    tracing::error!(
                        %project_id,
                        %session_id,
                        %response_message_id,
                        "Text queue message: as_text_message_for_processing returned None; acking and skipping"
                    );
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

            // Create the AI message row with status Generating as soon as we pick up the message,
            // so stop-generating can find it while the message is in the queue or retry delay.
            if let Err(e) = ai_message::ensure_ai_message_created_and_publish(
                &pool,
                &redis_conn,
                session_id,
                response_message_id,
                user_id,
                project_id,
                agent.clone(),
                payload.invoking_message_id,
            )
            .await
            {
                tracing::warn!(
                    %session_id,
                    %response_message_id,
                    "Failed to ensure AI message row when picking up: {}",
                    e
                );
            }

            let retry_num = payload.retry_num;
            let delivery_data = delivery.data.clone();
            let retry_routing_key = routing_key.clone();
            let retry_policy = chat_retry_policy();
            let dead_letter_spec = chat_dead_letter_spec();

            // Await here (not tokio::spawn): ack must follow processing for RabbitMQ x-consumer-timeout,
            // and prefetch=1 already serializes deliveries until we ack.
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
                agent.clone(),
                payload.invoking_message_id,
            )
            .await;

            match &result {
                Ok(outcome) => {
                    // Continuation is scheduled when LLM returned tool calls (tools eventually call
                    // maybe_complete_invoking_message), or when we re-queue a stuck nested sub-agent below.
                    let mut will_schedule_continuation = matches!(
                        outcome,
                        GenerateOutcome::Completed(p)
                            if p.requested_tool_calls.as_ref().is_some_and(|c| !c.is_empty())
                    );
                    if !will_schedule_continuation
                        && payload.invoking_message_id.is_some()
                        && payload.sub_agent_stuck_continuation_count
                            < MAX_SUB_AGENT_STUCK_CONTINUATIONS
                        && matches!(
                            agent,
                            ChatAgentType::ArchitectureAnalyzer
                                | ChatAgentType::SolutionAnalyzer
                                | ChatAgentType::Developer
                        )
                        && matches!(
                            outcome,
                            GenerateOutcome::Completed(p)
                                if p
                                    .requested_tool_calls
                                    .as_ref().is_none_or(|c| c.is_empty())
                        )
                    {
                        let next_count =
                            payload.sub_agent_stuck_continuation_count.saturating_add(1);
                        match serde_json::to_value(ChatMessagePayload::Text(TextPayload {
                            text: SUB_AGENT_STUCK_NUDGE.to_string(),
                            requested_tool_calls: None,
                            thinking_blocks: None,
                        })) {
                            Ok(nudge_json) => {
                                let queue_message = ChatSessionQueueMessage {
                                    user_id,
                                    role: ChatSessionRoleType::Ai,
                                    type_: ChatMessageType::Text,
                                    payload: Some(nudge_json),
                                    response_message_id: new_uuid(),
                                    retry_num: 0,
                                    invoking_message_id: payload.invoking_message_id,
                                    agent: agent.clone(),
                                    sub_agent_stuck_continuation_count: next_count,
                                };
                                match chat_message_queue::publish_with_publisher(
                                    &rabbit_publisher,
                                    project_id,
                                    session_id,
                                    &queue_message,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        will_schedule_continuation = true;
                                        tracing::warn!(
                                            %project_id,
                                            %session_id,
                                            ?agent,
                                            next_count,
                                            "Re-queued nested sub-agent after completion with no tool_calls"
                                        );
                                    }
                                    Err(e) => tracing::error!(
                                        %project_id,
                                        %session_id,
                                        error = %e,
                                        "Failed to publish sub-agent stuck continuation"
                                    ),
                                }
                            }
                            Err(e) => tracing::error!(
                                %project_id,
                                %session_id,
                                error = %e,
                                "Failed to serialize sub-agent stuck nudge payload"
                            ),
                        }
                    }
                    let should_publish_release_probe = matches!(agent, ChatAgentType::Planner)
                        && payload.invoking_message_id.is_none()
                        && matches!(outcome, GenerateOutcome::Completed(_))
                        && !will_schedule_continuation;
                    generation_lifecycle::notify_if_generation_loop_finished(
                        &redis_conn,
                        user_id,
                        project_id,
                        session_id,
                        will_schedule_continuation,
                    )
                    .await;
                    if should_publish_release_probe {
                        release_controller_box::publish_release_controller_probe(
                            &rabbit_publisher,
                            user_id,
                            project_id,
                            session_id,
                        )
                        .await;
                    }
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
                        // Update DB first so on reload the user sees Failed; then notify client.
                        set_message_failed_and_notify(
                            &pool,
                            &redis_conn,
                            user_id,
                            project_id,
                            session_id,
                            response_message_id,
                            agent.clone(),
                        )
                        .await;
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
                                    tracing::error!(
                                        "Failed to parse message for retry: {}",
                                        parse_e
                                    );
                                    set_message_failed_and_notify(
                                        &pool,
                                        &redis_conn,
                                        user_id,
                                        project_id,
                                        session_id,
                                        response_message_id,
                                        agent.clone(),
                                    )
                                    .await;
                                    if let Err(dl_e) = publish_to_dead_letter(
                                        &retry_channel,
                                        &dead_letter_spec.exchange_name,
                                        &retry_routing_key,
                                        &delivery_data,
                                    )
                                    .await
                                    {
                                        tracing::error!(
                                            "Failed to publish to dead letter: {}",
                                            dl_e
                                        );
                                    }
                                    if delivery.acker.ack(BasicAckOptions::default()).await.is_err()
                                    {
                                        tracing::warn!("Failed to ack delivery");
                                    }
                                    continue 'consumer;
                                }
                            };
                        payload_for_retry.retry_num = next_retry_num;
                        let payload_bytes = match serde_json::to_vec(&payload_for_retry) {
                            Ok(b) => b,
                            Err(ser_e) => {
                                tracing::error!(
                                    "Failed to serialize message for retry: {}",
                                    ser_e
                                );
                                set_message_failed_and_notify(
                                    &pool,
                                    &redis_conn,
                                    user_id,
                                    project_id,
                                    session_id,
                                    response_message_id,
                                    agent.clone(),
                                )
                                .await;
                                if delivery.acker.ack(BasicAckOptions::default()).await.is_err() {
                                    tracing::warn!("Failed to ack delivery");
                                }
                                continue 'consumer;
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
                            set_message_failed_and_notify(
                                &pool,
                                &redis_conn,
                                user_id,
                                project_id,
                                session_id,
                                response_message_id,
                                agent.clone(),
                            )
                            .await;
                        }
                    }
                }
            }

            if delivery.acker.ack(BasicAckOptions::default()).await.is_err() {
                tracing::warn!("Failed to ack delivery");
            }
        } else if payload.type_ == ChatMessageType::ReleaseControllerBox {
            let user_id = payload.user_id;
            if let Err(e) = release_controller_box::consume_release_controller_queue_message(
                &pool,
                &redis_conn,
                user_id,
                project_id,
                session_id,
                response_message_id,
            )
            .await
            {
                tracing::error!(
                    %project_id,
                    %session_id,
                    %response_message_id,
                    "release_controller_box consumer: {}",
                    e
                );
            }
            if delivery.acker.ack(BasicAckOptions::default()).await.is_err() {
                tracing::warn!("Failed to ack delivery");
            }
        } else {
            // Non-TEXT: create message in DB, publish Redis "new message created", then type-specific handler.
            let user_id = payload.user_id;
            let chat_payload = match payload.typed_payload() {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(
                        %response_message_id,
                        %project_id,
                        %session_id,
                        queue_message_type = ?payload.type_,
                        error = %e,
                        "runner: queue payload JSON does not match message type; acking and skipping"
                    );
                    let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                    continue;
                }
            };
            tracing::info!(
                target: "baal_backend_actix::tool_queue",
                %response_message_id,
                %project_id,
                %session_id,
                queue_message_type = ?payload.type_,
                chat_payload_is_none = chat_payload.is_none(),
                chat_payload_variant = chat_payload.as_ref().map(|p| p.variant_label()),
                "runner: non-TEXT queue message — about to insert DB row and spawn tool handler"
            );
            let payload_json_for_insert = chat_payload.as_ref().and_then(|p| {
                match serde_json::to_value(p) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::error!(
                            %response_message_id,
                            message_type = ?payload.type_,
                            "Failed to serialize ChatMessagePayload for DB insert (handler still runs with typed payload): {}",
                            e
                        );
                        None
                    }
                }
            });
            let role = payload.role;
            let type_ = payload.type_;
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
            let event =
                WsServerEvent::ChatSessionMessageCreated(ChatSessionMessageCreatedPayload {
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
                payload.type_,
                ToolMessageContext {
                    pool: pool.clone(),
                    redis_conn: redis_conn.clone(),
                    project_id,
                    session_id,
                    message_id: response_message_id,
                    user_id,
                    message_type: payload.type_,
                    payload: chat_payload,
                    invoking_message_id: payload.invoking_message_id,
                    rabbit_publisher: Some(rabbit_publisher.clone()),
                },
            );
            let _ = delivery.acker.ack(BasicAckOptions::default()).await;
        }
    }

    Ok(())
}
