use crate::database::{block_on_db_tokio, DbPool};
use crate::libs::unit_slug::compose_unit_id;
use crate::models::chat_messages::ChatMessageStatus;
use crate::models::projects::{ProjectUnitInsert, ProjectUnitResponse, ProjectUnitRow};
use crate::routes::ws::events::{ProjectUnitEventPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::project_units;
use crate::services::chat_worker::db::load_units_from_latest_production_metadata;
use crate::services::chat_worker::llm::tools::UnitSubTaskStatus;
use crate::services::message_queue::SharedRabbitPublisher;
use crate::services::unit_technical_reviewer::helpers::{
    broadcast_message_updated, compile_ts_to_js, extract_modify_unit_payload,
    extract_unit_dependencies, load_tool_message_row, run_rule_check, trigger_llm_continuation,
    update_tool_message_payload, BoxError, RedisConn, UnitOperation,
};
use crate::services::unit_technical_reviewer::type_checker::run_typescript_check;
use crate::services::unit_technical_reviewer::queue::{
    publish_reviewer_message, UnitReviewerMessageType, UnitReviewerQueueMessage,
};
use crate::services::units::{build_unit_metadata, UnitMetadataBuildInput};
use diesel::prelude::*;
use std::sync::Arc;


pub async fn process_modify_unit_compiler_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_modify_unit_payload(&row)?;

    match run_typescript_check(pool, message.project_id, message.session_id, &message.code).await {
        Ok(()) => {
            payload.compiler_check_sub_task = UnitSubTaskStatus::Succeeded;
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
            let next = UnitReviewerQueueMessage {
                message_type: UnitReviewerMessageType::ModifyUnitRuleCheck,
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

pub async fn process_modify_unit_rule_check(
    pool: &DbPool,
    redis_conn: &RedisConn,
    publisher: &Arc<SharedRabbitPublisher>,
    message: UnitReviewerQueueMessage,
) -> Result<(), BoxError> {
    let tool_message_id = message.tool_message_id;
    let row = load_tool_message_row(pool, tool_message_id).await?;
    let invoking_message_id = row.invoking_message_id;
    let mut payload = extract_modify_unit_payload(&row)?;

    // Load the parent unit first — needed for both old_code (passed to rule check) and the
    // new-revision insert that follows if all checks pass.
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

    let old_code = parent_opt.as_ref().and_then(|p| p.code.as_deref());
    let rule_errors =
        run_rule_check(UnitOperation::Modify, &message.unit_type, &message.code, old_code);

    if rule_errors.is_empty() {
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
                let compiled_code = compile_ts_to_js(&code).ok();
                let depends_on = extract_unit_dependencies(&code);
                let metadata = build_unit_metadata(
                    parent.unit_type,
                    pool,
                    message.project_id,
                    message.session_id,
                    UnitMetadataBuildInput {
                        code: &code,
                        api_method: payload.args.api_method.clone(),
                    },
                )
                .await;
                let insert = ProjectUnitInsert {
                    project_id: message.project_id,
                    chat_session_id: message.session_id,
                    id: new_id,
                    revision: new_rev,
                    code: Some(code),
                    compiled_code,
                    explanation: Some(explanation),
                    unit_type: parent.unit_type,
                    is_deleted: false,
                    context_slug: parent.context_slug.clone(),
                    module_slug: parent.module_slug.clone(),
                    unit_name_slug: parent.unit_name_slug.clone(),
                    metadata,
                    depends_on,
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
