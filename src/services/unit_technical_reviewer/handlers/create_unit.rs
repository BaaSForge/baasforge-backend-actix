use crate::database::{block_on_db_tokio, DbPool};
use crate::libs::unit_slug::compose_unit_id;
use crate::models::chat_messages::ChatMessageStatus;
use crate::models::projects::{ProjectUnitInsert, ProjectUnitResponse, ProjectUnitRow};
use crate::routes::ws::events::{ProjectUnitEventPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::project_units;
use crate::services::chat_worker::llm::tools::UnitSubTaskStatus;
use crate::services::message_queue::SharedRabbitPublisher;
use crate::services::unit_technical_reviewer::helpers::{
    broadcast_message_updated, extract_create_unit_payload, load_tool_message_row,
    run_rule_check, trigger_llm_continuation, update_tool_message_payload, BoxError, RedisConn,
    UnitOperation,
};
use crate::services::unit_technical_reviewer::type_checker::run_typescript_check;
use crate::services::unit_technical_reviewer::queue::{
    publish_reviewer_message, UnitReviewerMessageType, UnitReviewerQueueMessage,
};
use diesel::prelude::*;
use std::sync::Arc;

pub async fn process_create_unit_compiler_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_create_unit_payload(&row)?;

    match run_typescript_check(pool, message.project_id, message.session_id, &message.code).await {
        Ok(()) => {
            payload.compiler_check_sub_task = UnitSubTaskStatus::Succeeded;
            // Status stays Generating; rule check follows.
            let updated_row = update_tool_message_payload(
                pool,
                tool_message_id,
                &payload,
                ChatMessageStatus::Generating,
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
            // Dispatch rule check as the next step.
            let next = UnitReviewerQueueMessage {
                message_type: UnitReviewerMessageType::CreateUnitRuleCheck,
                ..message
            };
            publish_reviewer_message(publisher, &next)
                .await
                .map_err(|e| -> BoxError { e.to_string().into() })?;
        }
        Err(error_msg) => {
            payload.compiler_check_sub_task = UnitSubTaskStatus::Failed;
            payload.error = Some(error_msg);
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
    }

    Ok(())
}

pub async fn process_create_unit_rule_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_create_unit_payload(&row)?;

    let rule_errors = run_rule_check(UnitOperation::Create, &message.unit_type, &message.code, None);

    if rule_errors.is_empty() {
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
        payload.error = Some(format!(
            "Rule check failed. Fix the following violations:\n{}",
            rule_errors.join("\n")
        ));
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
