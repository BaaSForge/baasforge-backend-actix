use crate::database::PooledConn;
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageRow, ChatMessageType, ChatSessionRoleType,
};
use crate::models::projects::PlatformEnvironmentType;
use crate::schema::{
    chat_messages, chat_sessions, project_releases, project_runtimes, project_units, projects,
};
use crate::services::releases::all_live_production_release_ids;
use diesel::prelude::*;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug)]
pub enum TruncateSessionFromMessageOutcome {
    Success {
        removed_message_ids: Vec<Uuid>,
        /// `(runtime_id, cleared_stage_release_id)` for NATS `publish_stage_release_cleared`.
        stage_runtime_nats: Vec<(Uuid, Uuid)>,
    },
    SessionNotFound,
    AnchorNotFound,
    AnchorNotUserMessage,
    HasLiveProductionReleaseAfterAnchor,
}

pub fn truncate_session_from_user_message(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    user_message_id: Uuid,
) -> Result<TruncateSessionFromMessageOutcome, diesel::result::Error> {
    conn.transaction(|conn| {
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
            .filter(chat_messages::id.eq(user_message_id))
            .filter(chat_messages::chat_session_id.eq(session_id))
            .select(ChatMessageRow::as_select())
            .first(conn)
            .optional()?;

        let Some(anchor) = anchor else {
            return Ok(TruncateSessionFromMessageOutcome::AnchorNotFound);
        };
        if anchor.role != ChatSessionRoleType::User {
            return Ok(TruncateSessionFromMessageOutcome::AnchorNotUserMessage);
        }

        let live_prod_ids = all_live_production_release_ids(conn, project_id)?;
        let release_controllers_after_anchor: Vec<ChatMessageRow> = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(chat_messages::type_.eq(ChatMessageType::ReleaseControllerBox))
            .filter(chat_messages::created_at.gt(anchor.created_at))
            .select(ChatMessageRow::as_select())
            .load(conn)?;

        for row in release_controllers_after_anchor {
            let Ok(Some(ChatMessagePayload::ReleaseControllerBox(p))) =
                ChatMessagePayload::from_value(&row.type_, row.payload.clone())
            else {
                continue;
            };
            if let Some(pr) = p.prod_release {
                if live_prod_ids.contains(&pr.release.id) {
                    return Ok(
                        TruncateSessionFromMessageOutcome::HasLiveProductionReleaseAfterAnchor,
                    );
                }
            }
        }

        let to_delete: Vec<ChatMessageRow> = chat_messages::table
            .filter(chat_messages::chat_session_id.eq(session_id))
            .filter(
                chat_messages::created_at
                    .gt(anchor.created_at)
                    .or(chat_messages::created_at
                        .eq(anchor.created_at)
                        .and(chat_messages::id.ge(anchor.id))),
            )
            .order((chat_messages::created_at.asc(), chat_messages::id.asc()))
            .select(ChatMessageRow::as_select())
            .load(conn)?;

        let mut stage_release_ids: HashSet<Uuid> = HashSet::new();
        let mut prod_release_ids: HashSet<Uuid> = HashSet::new();
        let mut unit_ids: HashSet<String> = HashSet::new();

        for row in &to_delete {
            let Ok(payload_opt) = ChatMessagePayload::from_value(&row.type_, row.payload.clone())
            else {
                continue;
            };
            let Some(payload) = payload_opt else {
                continue;
            };
            match payload {
                ChatMessagePayload::ReleaseControllerBox(p) => {
                    if let Some(s) = p.stage_release {
                        stage_release_ids.insert(s.release.id);
                    }
                    if let Some(pr) = p.prod_release {
                        if !live_prod_ids.contains(&pr.release.id) {
                            prod_release_ids.insert(pr.release.id);
                        }
                    }
                }
                ChatMessagePayload::RejectControllerBox(p) => {
                    for u in p.unit_changes {
                        unit_ids.insert(u.id.clone());
                    }
                }
                _ => {}
            }
        }

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

        // Non-live production rows only: no prod runtime should reference them; FK ON DELETE SET NULL
        // covers any stray pointer if the DB ever disagreed with the live-id check above.
        for pid in prod_release_ids {
            diesel::delete(
                project_releases::table
                    .filter(project_releases::project_id.eq(project_id))
                    .filter(project_releases::id.eq(pid))
                    .filter(project_releases::is_stage.eq(false)),
            )
            .execute(conn)?;
        }

        if !unit_ids.is_empty() {
            let ids: Vec<String> = unit_ids.into_iter().collect();
            diesel::delete(
                project_units::table
                    .filter(project_units::project_id.eq(project_id))
                    .filter(project_units::chat_session_id.eq(session_id))
                    .filter(project_units::id.eq_any(ids)),
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
    })
}
