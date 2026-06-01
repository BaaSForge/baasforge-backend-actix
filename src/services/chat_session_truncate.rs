use crate::database::{BlockingDbError, DbPool, PooledConn, block_on_db};
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageRow, ChatMessageStatus, ChatMessageType,
    ChatSessionRoleType,
};
use crate::models::projects::PlatformEnvironmentType;
use crate::schema::{
    chat_messages, chat_sessions, project_releases, project_runtimes, projects,
};
use crate::services::chat_worker::db::{
    TruncateKeptToolMessagesBound, delete_session_draft_units_removed_by_truncate,
};
use crate::services::releases::all_live_production_release_ids;
use crate::services::runtime_nats::RuntimeNatsPublisher;
use diesel::prelude::*;
use std::collections::HashSet;
use uuid::Uuid;

macro_rules! messages_from_anchor_including {
    ($anchor:expr) => {
        chat_messages::created_at
            .gt($anchor.created_at)
            .or(chat_messages::created_at
                .eq($anchor.created_at)
                .and(chat_messages::id.ge($anchor.id)))
    };
}

macro_rules! messages_strictly_after_anchor {
    ($anchor:expr) => {
        chat_messages::created_at
            .gt($anchor.created_at)
            .or(chat_messages::created_at
                .eq($anchor.created_at)
                .and(chat_messages::id.gt($anchor.id)))
    };
}

#[derive(Debug)]
pub enum TruncateSessionFromMessageOutcome {
    Success {
        removed_message_ids: Vec<Uuid>,
        stage_runtime_nats: Vec<(Uuid, Uuid)>,
    },
    SessionNotFound,
    AnchorNotFound,
    AnchorNotUserMessage,
    HasLiveProductionReleaseAfterAnchor,
}

#[derive(Debug)]
pub enum RetryFailedPlannerTruncateOutcome {
    SessionNotFound,
    AlreadyGenerating,
    MessageNotFound,
    NotEligible { reason: &'static str },
    HasLiveProductionReleaseAfterMessage,
    Ok,
}

#[derive(Clone, Copy)]
struct TruncateAnchorPolicy {
    include_anchor: bool,
    require_user_role: bool,
}

pub struct TruncateNatsContext<'a> {
    pub publisher: Option<&'a RuntimeNatsPublisher>,
    pub subject: &'a str,
    pub project_id: Uuid,
}

pub async fn publish_truncate_stage_cleared_nats(
    ctx: &TruncateNatsContext<'_>,
    stage_runtime_nats: &[(Uuid, Uuid)],
) {
    let Some(nats) = ctx.publisher else {
        return;
    };
    if stage_runtime_nats.is_empty() {
        return;
    }
    for &(runtime_id, cleared_stage_release_id) in stage_runtime_nats {
        if let Err(e) = nats
            .publish_stage_release_cleared(
                ctx.subject,
                ctx.project_id,
                runtime_id,
                Some(cleared_stage_release_id),
            )
            .await
        {
            tracing::warn!(
                project_id = %ctx.project_id,
                %runtime_id,
                "truncate: NATS publish stage_release_cleared failed: {}",
                e
            );
        }
    }
}

pub async fn publish_truncate_outcome_nats(
    ctx: &TruncateNatsContext<'_>,
    outcome: &TruncateSessionFromMessageOutcome,
) {
    if let TruncateSessionFromMessageOutcome::Success {
        stage_runtime_nats,
        ..
    } = outcome
    {
        publish_truncate_stage_cleared_nats(ctx, stage_runtime_nats).await;
    }
}

/// DB truncate only. Must run inside an active Diesel transaction.
fn truncate_session_from_message_impl(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    anchor_message_id: Uuid,
    policy: TruncateAnchorPolicy,
) -> Result<TruncateSessionFromMessageOutcome, diesel::result::Error> {
    let session_ok = chat_sessions::table
        .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .filter(chat_sessions::id.eq(session_id))
        .select(chat_sessions::id)
        .first::<Uuid>(conn)
        .optional()?;
    if session_ok.is_none() {
        return Ok(TruncateSessionFromMessageOutcome::SessionNotFound);
    }

    let anchor: Option<ChatMessageRow> = chat_messages::table
        .filter(chat_messages::id.eq(anchor_message_id))
        .filter(chat_messages::chat_session_id.eq(session_id))
        .select(ChatMessageRow::as_select())
        .first(conn)
        .optional()?;

    let Some(anchor) = anchor else {
        return Ok(TruncateSessionFromMessageOutcome::AnchorNotFound);
    };
    if policy.require_user_role && anchor.role != ChatSessionRoleType::User {
        return Ok(TruncateSessionFromMessageOutcome::AnchorNotUserMessage);
    }

    let live_prod_ids = all_live_production_release_ids(conn, project_id)?;

    let release_controllers_after_anchor: Vec<ChatMessageRow> = if policy.include_anchor {
        chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(chat_messages::type_.eq(ChatMessageType::ReleaseControllerBox))
            .filter(messages_from_anchor_including!(&anchor))
            .select(ChatMessageRow::as_select())
            .load(conn)?
    } else {
        chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(chat_messages::type_.eq(ChatMessageType::ReleaseControllerBox))
            .filter(messages_strictly_after_anchor!(&anchor))
            .select(ChatMessageRow::as_select())
            .load(conn)?
    };

    for row in release_controllers_after_anchor {
        let Ok(Some(ChatMessagePayload::ReleaseControllerBox(p))) =
            ChatMessagePayload::from_value(&row.type_, row.payload.clone())
        else {
            continue;
        };
        if let Some(pr) = p.prod_release
            && live_prod_ids.contains(&pr.release.id)
        {
            return Ok(
                TruncateSessionFromMessageOutcome::HasLiveProductionReleaseAfterAnchor,
            );
        }
    }

    let to_delete: Vec<ChatMessageRow> = if policy.include_anchor {
        chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(messages_from_anchor_including!(&anchor))
            .order((chat_messages::created_at.asc(), chat_messages::id.asc()))
            .select(ChatMessageRow::as_select())
            .load(conn)?
    } else {
        chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(messages_strictly_after_anchor!(&anchor))
            .order((chat_messages::created_at.asc(), chat_messages::id.asc()))
            .select(ChatMessageRow::as_select())
            .load(conn)?
    };

    let mut stage_release_ids: HashSet<Uuid> = HashSet::new();
    let mut prod_release_ids: HashSet<Uuid> = HashSet::new();

    for row in &to_delete {
        let Ok(payload_opt) = ChatMessagePayload::from_value(&row.type_, row.payload.clone()) else {
            continue;
        };
        let Some(payload) = payload_opt else {
            continue;
        };
        if let ChatMessagePayload::ReleaseControllerBox(p) = payload {
            if let Some(s) = p.stage_release {
                stage_release_ids.insert(s.release.id);
            }
            if let Some(pr) = p.prod_release
                && !live_prod_ids.contains(&pr.release.id)
            {
                prod_release_ids.insert(pr.release.id);
            }
        }
    }

    let kept_tools = if policy.include_anchor {
        TruncateKeptToolMessagesBound::ToolsBeforeAnchor {
            anchor_created_at: anchor.created_at,
            anchor_message_id: anchor.id,
        }
    } else {
        TruncateKeptToolMessagesBound::ToolsThroughAnchor {
            anchor_created_at: anchor.created_at,
            anchor_message_id: anchor.id,
        }
    };
    delete_session_draft_units_removed_by_truncate(conn, project_id, session_id, kept_tools)?;

    let mut stage_runtime_nats: Vec<(Uuid, Uuid)> = Vec::new();

    for sid in stage_release_ids {
        let runtime_ids: Vec<Uuid> = project_runtimes::table
            .filter(project_runtimes::project_id.eq(project_id))
            .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Stage))
            .filter(project_runtimes::live_release_id.eq(Some(sid)))
            .select(project_runtimes::id)
            .load(conn)?;

        for rid in &runtime_ids {
            stage_runtime_nats.push((*rid, sid));
        }

        if !runtime_ids.is_empty() {
            diesel::update(
                project_runtimes::table
                    .filter(project_runtimes::project_id.eq(project_id))
                    .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Stage))
                    .filter(project_runtimes::live_release_id.eq(Some(sid))),
            )
            .set(project_runtimes::live_release_id.eq(None::<Uuid>))
            .execute(conn)?;
        }

        diesel::delete(
            project_releases::table
                .filter(project_releases::project_id.eq(project_id))
                .filter(project_releases::id.eq(sid))
                .filter(project_releases::is_stage.eq(true)),
        )
        .execute(conn)?;
    }

    for pid in prod_release_ids {
        diesel::delete(
            project_releases::table
                .filter(project_releases::project_id.eq(project_id))
                .filter(project_releases::id.eq(pid))
                .filter(project_releases::is_stage.eq(false)),
        )
        .execute(conn)?;
    }

    let removed_message_ids: Vec<Uuid> = to_delete.iter().map(|r| r.id).collect();

    if !removed_message_ids.is_empty() {
        diesel::delete(
            chat_messages::table
                .filter(chat_messages::chat_session_id.eq(session_id))
                .filter(chat_messages::id.eq_any(&removed_message_ids)),
        )
        .execute(conn)?;
    }

    Ok(TruncateSessionFromMessageOutcome::Success {
        removed_message_ids,
        stage_runtime_nats,
    })
}

pub fn truncate_session_from_user_message(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    user_message_id: Uuid,
) -> Result<TruncateSessionFromMessageOutcome, diesel::result::Error> {
    conn.transaction(|conn| {
        truncate_session_from_message_impl(
            conn,
            user_id,
            project_id,
            session_id,
            user_message_id,
            TruncateAnchorPolicy {
                include_anchor: true,
                require_user_role: true,
            },
        )
    })
}

fn truncate_session_strictly_after_message_impl(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    anchor_message_id: Uuid,
) -> Result<TruncateSessionFromMessageOutcome, diesel::result::Error> {
    truncate_session_from_message_impl(
        conn,
        user_id,
        project_id,
        session_id,
        anchor_message_id,
        TruncateAnchorPolicy {
            include_anchor: false,
            require_user_role: false,
        },
    )
}

pub fn truncate_session_strictly_after_message(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    anchor_message_id: Uuid,
) -> Result<TruncateSessionFromMessageOutcome, diesel::result::Error> {
    conn.transaction(|conn| {
        truncate_session_strictly_after_message_impl(
            conn,
            user_id,
            project_id,
            session_id,
            anchor_message_id,
        )
    })
}

fn truncate_strictly_after_planner_retry_in_tx(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
) -> Result<TruncateSessionFromMessageOutcome, diesel::result::Error> {
    let truncate_outcome = truncate_session_strictly_after_message_impl(
        conn,
        user_id,
        project_id,
        session_id,
        message_id,
    )?;
    if let TruncateSessionFromMessageOutcome::Success { .. } = &truncate_outcome {
        diesel::update(
            chat_messages::table
                .filter(chat_messages::id.eq(message_id))
                .filter(chat_messages::chat_session_id.eq(session_id)),
        )
        .set((
            chat_messages::status.eq(ChatMessageStatus::Generating),
            chat_messages::payload.eq(None::<serde_json::Value>),
        ))
        .execute(conn)?;
    }
    Ok(truncate_outcome)
}

struct RetryFailedPlannerTruncateDbResult {
    retry_outcome: RetryFailedPlannerTruncateOutcome,
    truncate_outcome: Option<TruncateSessionFromMessageOutcome>,
}

fn transaction_retry_failed_planner_truncate_and_reset(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
) -> Result<RetryFailedPlannerTruncateDbResult, diesel::result::Error> {
    conn.transaction(|conn| {
        let session_exists: Option<Uuid> = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first(conn)
            .optional()?;
        if session_exists.is_none() {
            return Ok(RetryFailedPlannerTruncateDbResult {
                retry_outcome: RetryFailedPlannerTruncateOutcome::SessionNotFound,
                truncate_outcome: None,
            });
        }

        let generating_count: i64 = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(chat_messages::role.eq(ChatSessionRoleType::Ai))
            .filter(chat_messages::status.eq(ChatMessageStatus::Generating))
            .count()
            .get_result(conn)?;
        if generating_count > 0 {
            return Ok(RetryFailedPlannerTruncateDbResult {
                retry_outcome: RetryFailedPlannerTruncateOutcome::AlreadyGenerating,
                truncate_outcome: None,
            });
        }

        let row: Option<ChatMessageRow> = chat_messages::table
            .filter(chat_messages::id.eq(message_id))
            .filter(chat_messages::chat_session_id.eq(session_id))
            .select(ChatMessageRow::as_select())
            .first(conn)
            .optional()?;
        let Some(row) = row else {
            return Ok(RetryFailedPlannerTruncateDbResult {
                retry_outcome: RetryFailedPlannerTruncateOutcome::MessageNotFound,
                truncate_outcome: None,
            });
        };
        if row.role != ChatSessionRoleType::Ai {
            return Ok(RetryFailedPlannerTruncateDbResult {
                retry_outcome: RetryFailedPlannerTruncateOutcome::NotEligible {
                    reason: "message is not an assistant row",
                },
                truncate_outcome: None,
            });
        }
        if row.type_ != ChatMessageType::Text {
            return Ok(RetryFailedPlannerTruncateDbResult {
                retry_outcome: RetryFailedPlannerTruncateOutcome::NotEligible {
                    reason: "message is not a text assistant message",
                },
                truncate_outcome: None,
            });
        }
        if row.status != ChatMessageStatus::Failed {
            return Ok(RetryFailedPlannerTruncateDbResult {
                retry_outcome: RetryFailedPlannerTruncateOutcome::NotEligible {
                    reason: "message status is not failed",
                },
                truncate_outcome: None,
            });
        }
        if row.agent != Some(ChatAgentType::Planner) {
            return Ok(RetryFailedPlannerTruncateDbResult {
                retry_outcome: RetryFailedPlannerTruncateOutcome::NotEligible {
                    reason: "message is not a planner assistant message",
                },
                truncate_outcome: None,
            });
        }
        if row.invoking_message_id.is_some() {
            return Ok(RetryFailedPlannerTruncateDbResult {
                retry_outcome: RetryFailedPlannerTruncateOutcome::NotEligible {
                    reason: "only main-thread planner messages can be retried via this endpoint",
                },
                truncate_outcome: None,
            });
        }

        let truncate_outcome = truncate_strictly_after_planner_retry_in_tx(
            conn,
            user_id,
            project_id,
            session_id,
            message_id,
        )?;

        let retry_outcome = match &truncate_outcome {
            TruncateSessionFromMessageOutcome::SessionNotFound => {
                RetryFailedPlannerTruncateOutcome::SessionNotFound
            }
            TruncateSessionFromMessageOutcome::AnchorNotFound => {
                RetryFailedPlannerTruncateOutcome::MessageNotFound
            }
            TruncateSessionFromMessageOutcome::AnchorNotUserMessage => {
                RetryFailedPlannerTruncateOutcome::MessageNotFound
            }
            TruncateSessionFromMessageOutcome::HasLiveProductionReleaseAfterAnchor => {
                RetryFailedPlannerTruncateOutcome::HasLiveProductionReleaseAfterMessage
            }
            TruncateSessionFromMessageOutcome::Success { .. } => {
                RetryFailedPlannerTruncateOutcome::Ok
            }
        };

        let truncate_outcome = match retry_outcome {
            RetryFailedPlannerTruncateOutcome::Ok => Some(truncate_outcome),
            _ => None,
        };

        Ok(RetryFailedPlannerTruncateDbResult {
            retry_outcome,
            truncate_outcome,
        })
    })
}

pub async fn truncate_session_from_user_message_with_nats(
    pool: &DbPool,
    ctx: TruncateNatsContext<'_>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    user_message_id: Uuid,
) -> Result<TruncateSessionFromMessageOutcome, BlockingDbError> {
    let outcome = block_on_db(pool, move |conn| {
        truncate_session_from_user_message(
            conn,
            user_id,
            project_id,
            session_id,
            user_message_id,
        )
    })
    .await?;
    publish_truncate_outcome_nats(&ctx, &outcome).await;
    Ok(outcome)
}

pub async fn transaction_retry_failed_planner_truncate_and_reset_with_nats(
    pool: &DbPool,
    ctx: TruncateNatsContext<'_>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
) -> Result<RetryFailedPlannerTruncateOutcome, BlockingDbError> {
    let db_result = block_on_db(pool, move |conn| {
        transaction_retry_failed_planner_truncate_and_reset(
            conn,
            user_id,
            project_id,
            session_id,
            message_id,
        )
    })
    .await?;

    if let Some(truncate_outcome) = db_result.truncate_outcome {
        publish_truncate_outcome_nats(&ctx, &truncate_outcome).await;
    }

    Ok(db_result.retry_outcome)
}
