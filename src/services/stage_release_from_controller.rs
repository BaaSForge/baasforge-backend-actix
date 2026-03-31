//! Create a stage `project_releases` row from a release-controller message and point a stage runtime at it.
//! Shared by [`crate::routes::chat::handlers::handle_create_stage_release`] and
//! [`crate::routes::project::handle_detach_runtime_live_release`] (controller variant).

use crate::database::PooledConn;
use crate::models::chat_messages::{ChatMessagePayload, ChatMessageRow, ChatMessageType};
use crate::models::projects::{
    PlatformEnvironmentType, ProjectReleaseResponse, ProjectReleaseWithBaseline, ProjectRuntimeResponse,
};
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::project_releases;
use crate::schema::project_runtimes;
use crate::schema::projects;
use crate::services::chat_worker::db::load_chat_session_draft_units_before_message_cutoff;
use crate::services::releases::{
    combined_units_from_release_metadata, compute_new_units,
    load_baseline_production_release_for_chat_session, load_latest_production_release_for_project,
    merge_combined_units, project_unit_refs_from_responses, revision_for_chat_release_controller,
    ReleaseUnitsMetadata,
};
use diesel::prelude::*;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageReleaseFromControllerError {
    SessionNotFound,
    MessageNotFound,
    NotReleaseController,
    ReleaseControllerPayloadMissing,
    ReleaseControllerPayloadInvalid,
    RuntimeNotFound,
    RuntimeNotStage,
    Metadata(String),
}

/// Same DB work as POST `.../release-controller-messages/{message_id}/stage-release` with
/// `stage_project_runtime_id` = `stage_runtime_id`.
pub fn transaction_create_stage_release_from_controller(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    stage_runtime_id: Uuid,
) -> Result<
    Result<(ProjectReleaseResponse, ChatMessageRow, Option<Uuid>), StageReleaseFromControllerError>,
    diesel::result::Error,
> {
    conn.transaction(|conn| {
        let session_ok = chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(chat_sessions::id.eq(session_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .first::<Uuid>(conn)
            .optional()?;
        if session_ok.is_none() {
            return Ok(Err(StageReleaseFromControllerError::SessionNotFound));
        }

        let msg_row: Option<ChatMessageRow> = chat_messages::table
            .filter(chat_messages::id.eq(message_id))
            .filter(chat_messages::chat_session_id.eq(session_id))
            .select(ChatMessageRow::as_select())
            .first(conn)
            .optional()?;
        let Some(msg_row) = msg_row else {
            return Ok(Err(StageReleaseFromControllerError::MessageNotFound));
        };
        if msg_row.type_ != ChatMessageType::ReleaseControllerBox {
            return Ok(Err(StageReleaseFromControllerError::NotReleaseController));
        }

        let mut box_payload = match ChatMessagePayload::from_value(
            &msg_row.type_,
            msg_row.payload.clone(),
        ) {
            Ok(Some(ChatMessagePayload::ReleaseControllerBox(p))) => p,
            Ok(None) => return Ok(Err(StageReleaseFromControllerError::ReleaseControllerPayloadMissing)),
            Ok(Some(other)) => {
                tracing::warn!(
                    message_id = %message_id,
                    variant = other.variant_label(),
                    "release_controller_box row has unexpected payload variant"
                );
                return Ok(Err(StageReleaseFromControllerError::ReleaseControllerPayloadInvalid));
            }
            Err(e) => {
                tracing::warn!(
                    message_id = %message_id,
                    error = %e,
                    "release_controller_box payload deserialize failed"
                );
                return Ok(Err(StageReleaseFromControllerError::ReleaseControllerPayloadInvalid));
            }
        };

        let stage_rt: Option<ProjectRuntimeResponse> = project_runtimes::table
            .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_runtimes::project_id.eq(project_id))
            .filter(project_runtimes::id.eq(stage_runtime_id))
            .select(ProjectRuntimeResponse::as_select())
            .first(conn)
            .optional()?;
        let Some(stage_rt) = stage_rt else {
            return Ok(Err(StageReleaseFromControllerError::RuntimeNotFound));
        };
        if stage_rt.environment != PlatformEnvironmentType::Stage {
            return Ok(Err(StageReleaseFromControllerError::RuntimeNotStage));
        }

        let previous_stage_release_id = stage_rt.live_release_id;

        let (session_base_prod_id, prev_release) =
            load_baseline_production_release_for_chat_session(conn, project_id, session_id)?;
        let base_for_row = match (&session_base_prod_id, &prev_release) {
            (Some(id), Some(_)) => Some(*id),
            _ => None,
        };
        // Merge against the **current** live production snapshot (not the session baseline).
        let previous_combined = match load_latest_production_release_for_project(conn, project_id)? {
            Some(r) if r.units_metadata.is_some() => {
                match combined_units_from_release_metadata(conn, project_id, r.units_metadata.as_ref())
                {
                    Ok(u) => u,
                    Err(e) => {
                        return Ok(Err(StageReleaseFromControllerError::Metadata(e.to_string())));
                    }
                }
            }
            _ => Vec::new(),
        };

        let session_tips = load_chat_session_draft_units_before_message_cutoff(
            conn,
            project_id,
            session_id,
            msg_row.created_at,
            message_id,
        )?;
        let new_units = compute_new_units(&previous_combined, &session_tips);
        let combined_units = merge_combined_units(&previous_combined, &session_tips);

        let meta = ReleaseUnitsMetadata {
            previous_unit_refs: project_unit_refs_from_responses(&previous_combined),
            new_unit_refs: project_unit_refs_from_responses(&new_units),
            combined_unit_refs: project_unit_refs_from_responses(&combined_units),
        };

        let revision = revision_for_chat_release_controller(conn, project_id, session_id, message_id, true)?;
        let inserted: ProjectReleaseResponse = diesel::insert_into(project_releases::table)
            .values((
                project_releases::project_id.eq(project_id),
                project_releases::revision.eq(revision),
                project_releases::units_metadata.eq(Some(meta)),
                project_releases::notes.eq(None::<String>),
                project_releases::chat_session_id.eq(Some(session_id)),
                project_releases::is_stage.eq(true),
                project_releases::base_production_release_id.eq(base_for_row),
            ))
            .returning(ProjectReleaseResponse::as_select())
            .get_result(conn)?;

        diesel::update(
            project_runtimes::table
                .filter(project_runtimes::id.eq(stage_runtime_id))
                .filter(project_runtimes::project_id.eq(project_id)),
        )
        .set(project_runtimes::live_release_id.eq(Some(inserted.id)))
        .execute(conn)?;

        box_payload.stage_release = Some(ProjectReleaseWithBaseline {
            release: inserted.clone(),
            base_production_release: prev_release.clone(),
        });
        let payload_json = match serde_json::to_value(&box_payload) {
            Ok(v) => v,
            Err(e) => return Ok(Err(StageReleaseFromControllerError::Metadata(e.to_string()))),
        };

        let updated_row = diesel::update(
            chat_messages::table.filter(chat_messages::id.eq(message_id)),
        )
        .set(chat_messages::payload.eq(Some(payload_json)))
        .returning(ChatMessageRow::as_returning())
        .get_result(conn)?;

        Ok(Ok((inserted, updated_row, previous_stage_release_id)))
    })
}
