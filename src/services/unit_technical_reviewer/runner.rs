use crate::configs::Settings;
use crate::database::{block_on_db_tokio, establish_pool, DbPool};
use crate::libs::unit_slug::compose_unit_id;
use crate::models::chat_messages::{ChatMessageResponse, ChatMessageRow, ChatMessageStatus};
use crate::models::projects::{ProjectUnitInsert, ProjectUnitResponse, ProjectUnitRow};
use crate::routes::ws::events::{
    ChatSessionMessageUpdatedPayload, ProjectUnitEventPayload, WsServerEvent,
};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::{chat_messages, project_units};
use crate::services::chat_worker::db::load_units_from_latest_production_metadata;
use crate::services::chat_worker::llm::tools::create_unit::ToolCreateUnitPayload;
use crate::services::chat_worker::llm::tools::modify_unit::ToolModifyUnitPayload;
use crate::services::chat_worker::llm::tools::remove_unit::ToolRemoveUnitPayload;
use crate::services::chat_worker::llm::tools::{maybe_complete_invoking_message, UnitSubTaskStatus};
use crate::services::message_queue::{create_shared_publisher, SharedRabbitPublisher};
use crate::services::unit_technical_reviewer::queue::{
    create_reviewer_work_consumer_on_connection, publish_reviewer_message,
    UnitReviewerMessageType, UnitReviewerQueueMessage, UNIT_REVIEWER_EXCHANGE,
};
use diesel::prelude::*;
use lapin::options::BasicAckOptions;
use lapin::ConnectionProperties;
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::info;
use uuid::Uuid;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type RedisConn = Arc<Mutex<MultiplexedConnection>>;

pub async fn run() -> Result<(), BoxError> {
    tracing_subscriber::fmt::init();

    let settings = Settings::new()
        .map_err(|e| -> BoxError { e.to_string().into() })?;

    let redis_client = redis::Client::open(settings.redis_url.as_str())?;
    let redis_conn = redis_client.get_multiplexed_async_connection().await?;
    let redis_conn = Arc::new(Mutex::new(redis_conn));
    info!("Connected to Redis");

    let pool = establish_pool(
        settings.pg_database_url.clone(),
        settings.database_pool_size,
        settings.database_connection_timeout_secs,
    );

    let rabbit_url = settings.rabbit_url.clone();
    info!("Connecting to RabbitMQ...");
    let connection = lapin::Connection::connect(&rabbit_url, ConnectionProperties::default())
        .await
        .map_err(|e| -> BoxError { e.into() })?;
    let (_consumer_channel, mut consumer) =
        create_reviewer_work_consumer_on_connection(&connection)
            .await
            .map_err(|e| -> BoxError { e.into() })?;

    // Single publisher: used both for dispatching back to chat_session_exchange
    // (via maybe_complete_invoking_message → publish_with_publisher which hardcodes CHAT_SESSION_EXCHANGE)
    // and for dispatching next reviewer checks (to UNIT_REVIEWER_EXCHANGE via publish_reviewer_message).
    // publish_with_channel / publish_with_publisher both accept an exchange name at call time.
    let publisher = create_shared_publisher(&rabbit_url, UNIT_REVIEWER_EXCHANGE)
        .await
        .map_err(|e| -> BoxError { e.into() })?;

    info!("Unit technical reviewer started. Consuming from reviewer work queue.");

    while let Some(delivery_result) = consumer.next().await {
        let delivery = match delivery_result {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Consumer error: {}", e);
                return Err(std::io::Error::other(format!("consumer error: {e}")).into());
            }
        };

        let message = match serde_json::from_slice::<UnitReviewerQueueMessage>(&delivery.data) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Invalid reviewer message body: {}", e);
                let _ = delivery.acker.ack(BasicAckOptions::default()).await;
                continue;
            }
        };

        tracing::info!(
            tool_message_id = %message.tool_message_id,
            message_type = ?message.message_type,
            "Unit reviewer: processing message"
        );

        let result = process_message(&pool, &redis_conn, &publisher, message).await;
        if let Err(e) = result {
            tracing::error!("Unit reviewer message processing failed: {}", e);
        }

        if delivery.acker.ack(BasicAckOptions::default()).await.is_err() {
            tracing::warn!("Failed to ack delivery");
        }
    }

    Ok(())
}

async fn process_message(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    match message.message_type {
        UnitReviewerMessageType::CreateUnitCompilerCheck => {
            process_create_unit_compiler_check(pool, redis_conn, publisher, message).await
        }
        UnitReviewerMessageType::CreateUnitRuleCheck => {
            process_create_unit_rule_check(pool, redis_conn, publisher, message).await
        }
        UnitReviewerMessageType::ModifyUnitCompilerCheck => {
            process_modify_unit_compiler_check(pool, redis_conn, publisher, message).await
        }
        UnitReviewerMessageType::ModifyUnitRuleCheck => {
            process_modify_unit_rule_check(pool, redis_conn, publisher, message).await
        }
        UnitReviewerMessageType::RemoveUnitRuleCheck => {
            process_remove_unit_rule_check(pool, redis_conn, publisher, message).await
        }
    }
}

// ── create_unit: compiler check ─────────────────────────────────────────────

async fn process_create_unit_compiler_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_create_unit_payload(&row)?;

    // Simulate compiler check — code inspection will be added later.
    let check_succeeded = simulate_compiler_check(&message.code);

    if check_succeeded {
        payload.compiler_check_sub_task = UnitSubTaskStatus::Succeeded;
        // Status stays Generating; rule check follows.
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Generating)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
        // Dispatch rule check as the next step.
        let next = UnitReviewerQueueMessage {
            message_type: UnitReviewerMessageType::CreateUnitRuleCheck,
            ..message
        };
        publish_reviewer_message(publisher, &next)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
    } else {
        let error_msg = "Compiler check failed: code contains syntax errors.".to_string();
        payload.compiler_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some(error_msg);
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
        // Done (failed): let the LLM continue so it can react to the failure.
        trigger_llm_continuation(
            pool,
            redis_conn,
            publisher,
            invoking_message_id,
            message.user_id,
            message.project_id,
            message.session_id,
        )
        .await;
    }

    Ok(())
}

// ── create_unit: rule check ──────────────────────────────────────────────────

async fn process_create_unit_rule_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_create_unit_payload(&row)?;

    // Simulate rule check — linting / naming-convention checks will be added later.
    let check_succeeded = simulate_rule_check(&message.unit_type, &message.context_slug);

    if check_succeeded {
        // All checks passed: now insert the project_units row. The unit is persisted only at
        // this final step so that no partial or invalid unit ever lands in the database.
        let unit_id = compose_unit_id(
            &payload.args.context_slug,
            &payload.args.module_slug,
            payload.args.unit_type.as_slug(),
            &payload.args.unit_name_slug,
            1, // initial revision
        );
        let insert = ProjectUnitInsert {
            project_id: message.project_id,
            chat_session_id: message.session_id,
            id: unit_id,
            revision: 1,
            code: Some(payload.args.code.clone()),
            explanation: Some(payload.args.explanation.clone()),
            unit_type: payload.args.unit_type,
            is_deleted: false,
            context_slug: payload.args.context_slug.clone(),
            module_slug: payload.args.module_slug.clone(),
            unit_name_slug: payload.args.unit_name_slug.clone(),
        };
        match block_on_db_tokio(pool, move |conn| {
            diesel::insert_into(project_units::table)
                .values(&insert)
                .returning(ProjectUnitRow::as_returning())
                .get_result::<ProjectUnitRow>(conn)
        })
        .await
        {
            Ok(unit_row) => {
                let rev = unit_row.revision;
                let created_unit = ProjectUnitResponse::from_row(unit_row, rev);
                payload.rule_check_sub_task = UnitSubTaskStatus::Succeeded;
                payload.created_unit = Some(created_unit.clone());
                let updated_row = update_tool_message_payload(
                    pool,
                    tool_message_id,
                    &payload,
                    ChatMessageStatus::Completed,
                )
                .await?;
                // Broadcast updated tool message (sub_task statuses + created_unit now populated).
                broadcast_message_updated(
                    redis_conn,
                    message.user_id,
                    message.project_id,
                    message.session_id,
                    updated_row,
                )
                .await;
                // Broadcast ProjectUnitCreated only here, after the unit is actually in the DB.
                let channel = user_channel(message.user_id);
                let unit_event = WsServerEvent::ProjectUnitCreated(ProjectUnitEventPayload {
                    project_id: message.project_id,
                    session_id: message.session_id,
                    unit: created_unit,
                });
                let mut guard = redis_conn.lock().await;
                if let Err(e) =
                    publish_event_to_channel(&mut *guard, &channel, &unit_event).await
                {
                    tracing::warn!(
                        "unit reviewer: failed to broadcast ProjectUnitCreated: {}",
                        e
                    );
                }
            }
            Err(e) => {
                // Insert failed (e.g. race condition added a duplicate after pre_check).
                // Treat as rule_check failure so the LLM can react.
                tracing::error!(
                    tool_message_id = %tool_message_id,
                    "unit reviewer: create_unit insert failed after all checks passed: {}",
                    e
                );
                payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
                payload.error = Some(format!(
                    "Failed to persist unit after checks: {}",
                    e
                ));
                let updated_row = update_tool_message_payload(
                    pool,
                    tool_message_id,
                    &payload,
                    ChatMessageStatus::Completed,
                )
                .await?;
                broadcast_message_updated(
                    redis_conn,
                    message.user_id,
                    message.project_id,
                    message.session_id,
                    updated_row,
                )
                .await;
            }
        }
    } else {
        // Rule check failed — do not insert the unit.
        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some("Rule check failed: unit violates naming conventions.".to_string());
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
    }

    // Let the LLM continue in all cases (success and failure). The LLM sees the final
    // sub_task statuses and created_unit (or error) in its history and reacts accordingly.
    trigger_llm_continuation(
        pool,
        redis_conn,
        publisher,
        invoking_message_id,
        message.user_id,
        message.project_id,
        message.session_id,
    )
    .await;

    Ok(())
}

// ── modify_unit: compiler check ──────────────────────────────────────────────

async fn process_modify_unit_compiler_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_modify_unit_payload(&row)?;

    let check_succeeded = simulate_compiler_check(&message.code);

    if check_succeeded {
        payload.compiler_check_sub_task = UnitSubTaskStatus::Succeeded;
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Generating)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
        let next = UnitReviewerQueueMessage {
            message_type: UnitReviewerMessageType::ModifyUnitRuleCheck,
            ..message
        };
        publish_reviewer_message(publisher, &next)
            .await
            .map_err(|e| -> BoxError { e.to_string().into() })?;
    } else {
        payload.compiler_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some("Compiler check failed: code contains syntax errors.".to_string());
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
        trigger_llm_continuation(
            pool,
            redis_conn,
            publisher,
            invoking_message_id,
            message.user_id,
            message.project_id,
            message.session_id,
        )
        .await;
    }

    Ok(())
}

// ── modify_unit: rule check ──────────────────────────────────────────────────

async fn process_modify_unit_rule_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_modify_unit_payload(&row)?;

    let check_succeeded = simulate_rule_check(&message.unit_type, &message.context_slug);

    if check_succeeded {
        // Look up the parent unit row so we can compute the new revision and insert it.
        let parent_key = payload.args.unit_id.clone();
        let pid = message.project_id;
        let sid = message.session_id;
        let pk = parent_key.clone();
        let parent_opt = block_on_db_tokio(pool, move |conn| {
            let parent: Option<ProjectUnitRow> = project_units::table
                .filter(project_units::project_id.eq(pid))
                .filter(project_units::chat_session_id.eq(sid))
                .filter(project_units::id.eq(&pk))
                .select(ProjectUnitRow::as_select())
                .first(conn)
                .optional()?;
            if let Some(p) = parent {
                return Ok(Some(p));
            }
            let prod_units = load_units_from_latest_production_metadata(conn, pid)?;
            Ok(prod_units.into_iter().find(|u| u.id == pk && !u.is_deleted).map(|u| u.row))
        })
        .await
        .map_err(|e| -> BoxError { e.to_string().into() })?;

        match parent_opt {
            Some(parent) => {
                let new_rev = parent.revision + 1;
                let new_id = compose_unit_id(
                    &parent.context_slug,
                    &parent.module_slug,
                    parent.unit_type.as_slug(),
                    &parent.unit_name_slug,
                    new_rev,
                );
                let code = payload.args.code.clone();
                let explanation = payload.args.explanation.clone();
                let insert = ProjectUnitInsert {
                    project_id: message.project_id,
                    chat_session_id: message.session_id,
                    id: new_id,
                    revision: new_rev,
                    code: Some(code),
                    explanation: Some(explanation),
                    unit_type: parent.unit_type,
                    is_deleted: false,
                    context_slug: parent.context_slug.clone(),
                    module_slug: parent.module_slug.clone(),
                    unit_name_slug: parent.unit_name_slug.clone(),
                };
                match block_on_db_tokio(pool, move |conn| {
                    diesel::insert_into(project_units::table)
                        .values(&insert)
                        .returning(ProjectUnitRow::as_returning())
                        .get_result::<ProjectUnitRow>(conn)
                })
                .await
                {
                    Ok(unit_row) => {
                        let rev = unit_row.revision;
                        let revised_unit = ProjectUnitResponse::from_row(unit_row, rev);
                        payload.rule_check_sub_task = UnitSubTaskStatus::Succeeded;
                        payload.revised_unit = Some(revised_unit.clone());
                        let updated_row = update_tool_message_payload(
                            pool,
                            tool_message_id,
                            &payload,
                            ChatMessageStatus::Completed,
                        )
                        .await?;
                        broadcast_message_updated(
                            redis_conn,
                            message.user_id,
                            message.project_id,
                            message.session_id,
                            updated_row,
                        )
                        .await;
                        let channel = user_channel(message.user_id);
                        let unit_event = WsServerEvent::ProjectUnitModified(ProjectUnitEventPayload {
                            project_id: message.project_id,
                            session_id: message.session_id,
                            unit: revised_unit,
                        });
                        let mut guard = redis_conn.lock().await;
                        if let Err(e) =
                            publish_event_to_channel(&mut *guard, &channel, &unit_event).await
                        {
                            tracing::warn!(
                                "unit reviewer: failed to broadcast ProjectUnitModified: {}",
                                e
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            tool_message_id = %tool_message_id,
                            "unit reviewer: modify_unit insert failed after all checks passed: {}",
                            e
                        );
                        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
                        payload.error =
                            Some(format!("Failed to persist revised unit after checks: {}", e));
                        let updated_row = update_tool_message_payload(
                            pool,
                            tool_message_id,
                            &payload,
                            ChatMessageStatus::Completed,
                        )
                        .await?;
                        broadcast_message_updated(
                            redis_conn,
                            message.user_id,
                            message.project_id,
                            message.session_id,
                            updated_row,
                        )
                        .await;
                    }
                }
            }
            None => {
                // Parent disappeared or was already deleted between pre_check and now (race condition).
                tracing::error!(
                    tool_message_id = %tool_message_id,
                    "unit reviewer (modify_unit): expected parent to be present, but was None"
                );
            }
        }
    } else {
        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some("Rule check failed: unit violates naming conventions.".to_string());
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
    }

    trigger_llm_continuation(
        pool,
        redis_conn,
        publisher,
        invoking_message_id,
        message.user_id,
        message.project_id,
        message.session_id,
    )
    .await;

    Ok(())
}

// ── remove_unit: rule check ──────────────────────────────────────────────────

async fn process_remove_unit_rule_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_remove_unit_payload(&row)?;

    let check_succeeded = simulate_rule_check(&message.unit_type, &message.context_slug);

    if check_succeeded {
        // Look up the parent unit row so we can insert the deletion marker (is_deleted = true).
        let parent_key = payload.args.unit_id.clone();
        let pid = message.project_id;
        let sid = message.session_id;
        let pk = parent_key.clone();
        let parent_opt = block_on_db_tokio(pool, move |conn| {
            let parent: Option<ProjectUnitRow> = project_units::table
                .filter(project_units::project_id.eq(pid))
                .filter(project_units::chat_session_id.eq(sid))
                .filter(project_units::id.eq(&pk))
                .select(ProjectUnitRow::as_select())
                .first(conn)
                .optional()?;
            if let Some(p) = parent {
                return Ok(Some(p));
            }
            let prod_units = load_units_from_latest_production_metadata(conn, pid)?;
            Ok(prod_units.into_iter().find(|u| u.id == pk && !u.is_deleted).map(|u| u.row))
        })
        .await
        .map_err(|e| -> BoxError { e.to_string().into() })?;

        match parent_opt {
            Some(parent) if !parent.is_deleted => {
                let new_rev = parent.revision + 1;
                let new_id = compose_unit_id(
                    &parent.context_slug,
                    &parent.module_slug,
                    parent.unit_type.as_slug(),
                    &parent.unit_name_slug,
                    new_rev,
                );
                let explanation = parent.explanation.clone();
                let insert = ProjectUnitInsert {
                    project_id: message.project_id,
                    chat_session_id: message.session_id,
                    id: new_id,
                    revision: new_rev,
                    code: Some(String::new()),
                    explanation,
                    unit_type: parent.unit_type,
                    is_deleted: true,
                    context_slug: parent.context_slug.clone(),
                    module_slug: parent.module_slug.clone(),
                    unit_name_slug: parent.unit_name_slug.clone(),
                };
                match block_on_db_tokio(pool, move |conn| {
                    diesel::insert_into(project_units::table)
                        .values(&insert)
                        .returning(ProjectUnitRow::as_returning())
                        .get_result::<ProjectUnitRow>(conn)
                })
                .await
                {
                    Ok(unit_row) => {
                        let rev = unit_row.revision;
                        let removed_unit = ProjectUnitResponse::from_row(unit_row, rev);
                        payload.rule_check_sub_task = UnitSubTaskStatus::Succeeded;
                        payload.removed_unit = Some(removed_unit.clone());
                        let updated_row = update_tool_message_payload(
                            pool,
                            tool_message_id,
                            &payload,
                            ChatMessageStatus::Completed,
                        )
                        .await?;
                        broadcast_message_updated(
                            redis_conn,
                            message.user_id,
                            message.project_id,
                            message.session_id,
                            updated_row,
                        )
                        .await;
                        let channel = user_channel(message.user_id);
                        let unit_event = WsServerEvent::ProjectUnitDeleted(ProjectUnitEventPayload {
                            project_id: message.project_id,
                            session_id: message.session_id,
                            unit: removed_unit,
                        });
                        let mut guard = redis_conn.lock().await;
                        if let Err(e) =
                            publish_event_to_channel(&mut *guard, &channel, &unit_event).await
                        {
                            tracing::warn!(
                                "unit reviewer: failed to broadcast ProjectUnitDeleted: {}",
                                e
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            tool_message_id = %tool_message_id,
                            "unit reviewer: remove_unit insert failed after rule_check passed: {}",
                            e
                        );
                        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
                        payload.error =
                            Some(format!("Failed to persist deletion marker after checks: {}", e));
                        let updated_row = update_tool_message_payload(
                            pool,
                            tool_message_id,
                            &payload,
                            ChatMessageStatus::Completed,
                        )
                        .await?;
                        broadcast_message_updated(
                            redis_conn,
                            message.user_id,
                            message.project_id,
                            message.session_id,
                            updated_row,
                        )
                        .await;
                    }
                }
            }
            Some(_) /* is_deleted */ | None => {
                // Parent disappeared or was already deleted between pre_check and now (race condition).
                tracing::error!(
                    tool_message_id = %tool_message_id,
                    "unit reviewer (remove_unit): expected parent to be present, but was None"
                );
            }
        }
    } else {
        payload.rule_check_sub_task = UnitSubTaskStatus::Failed;
        payload.error = Some("Rule check failed: cannot remove this unit type.".to_string());
        let updated_row =
            update_tool_message_payload(pool, tool_message_id, &payload, ChatMessageStatus::Completed)
                .await?;
        broadcast_message_updated(
            redis_conn,
            message.user_id,
            message.project_id,
            message.session_id,
            updated_row,
        )
        .await;
    }

    trigger_llm_continuation(
        pool,
        redis_conn,
        publisher,
        invoking_message_id,
        message.user_id,
        message.project_id,
        message.session_id,
    )
    .await;

    Ok(())
}

// ── shared helpers ───────────────────────────────────────────────────────────

/// Simulates a compiler/syntax check on JavaScript code.
/// For now always succeeds; real AST/parse checks will replace this.
fn simulate_compiler_check(_code: &str) -> bool {
    true
}

/// Simulates a rule/convention check on the unit.
/// For now always succeeds; real linting rules will replace this.
fn simulate_rule_check(_unit_type: &str, _context_slug: &str) -> bool {
    true
}

async fn load_tool_message_row(pool: &DbPool, tool_message_id: Uuid) -> Result<ChatMessageRow, BoxError> {
    block_on_db_tokio(pool, move |conn| {
        chat_messages::table
            .filter(chat_messages::id.eq(tool_message_id))
            .select(ChatMessageRow::as_select())
            .first::<ChatMessageRow>(conn)
    })
    .await
    .map_err(|e| -> BoxError { e.to_string().into() })
}

fn extract_create_unit_payload(row: &ChatMessageRow) -> Result<ToolCreateUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("create_unit tool message has no payload")?;
    serde_json::from_value::<ToolCreateUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolCreateUnitPayload: {e}").into() })
}

fn extract_modify_unit_payload(row: &ChatMessageRow) -> Result<ToolModifyUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("modify_unit tool message has no payload")?;
    serde_json::from_value::<ToolModifyUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolModifyUnitPayload: {e}").into() })
}

fn extract_remove_unit_payload(row: &ChatMessageRow) -> Result<ToolRemoveUnitPayload, BoxError> {
    let value = row
        .payload
        .clone()
        .ok_or("remove_unit tool message has no payload")?;
    serde_json::from_value::<ToolRemoveUnitPayload>(value)
        .map_err(|e| -> BoxError { format!("deserialize ToolRemoveUnitPayload: {e}").into() })
}

async fn update_tool_message_payload<P: serde::Serialize>(
    pool: &DbPool,
    tool_message_id: Uuid,
    payload: &P,
    status: ChatMessageStatus,
) -> Result<ChatMessageRow, BoxError> {
    let payload_value = serde_json::to_value(payload)
        .map_err(|e| -> BoxError { format!("serialize reviewer payload: {e}").into() })?;
    block_on_db_tokio(pool, move |conn| {
        diesel::update(chat_messages::table.filter(chat_messages::id.eq(tool_message_id)))
            .set((
                chat_messages::payload.eq(Some(payload_value)),
                chat_messages::status.eq(status),
            ))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
    })
    .await
    .map_err(|e| -> BoxError { e.to_string().into() })
}

async fn broadcast_message_updated(
    redis_conn: &RedisConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    row: ChatMessageRow,
) {
    let channel = user_channel(user_id);
    let message: ChatMessageResponse = row.into();
    let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
        project_id,
        session_id,
        message,
    });
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
        tracing::warn!("unit reviewer: failed to broadcast ChatSessionMessageUpdated: {}", e);
    }
}

/// Calls `maybe_complete_invoking_message` so the AI Text invoker is completed and the LLM
/// receives a queue continuation. Should be called whenever a tool message is set to Completed
/// (success or failure) in the reviewer — in both cases the LLM must be allowed to continue.
async fn trigger_llm_continuation(
    pool: &DbPool,
    redis_conn: &RedisConn,
    chat_publisher: &Arc<SharedRabbitPublisher>,
    invoking_message_id: Option<Uuid>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
) {
    let Some(inv_id) = invoking_message_id else {
        tracing::warn!(
            "unit reviewer: invoking_message_id is None; cannot trigger LLM continuation"
        );
        return;
    };
    maybe_complete_invoking_message(
        pool,
        redis_conn,
        Some(chat_publisher),
        inv_id,
        user_id,
        project_id,
        session_id,
        true,
    )
    .await;
}
