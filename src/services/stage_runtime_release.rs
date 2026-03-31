//! Shared DB logic for pointing a stage project runtime at a stage release revision, or clearing it.

use crate::database::PooledConn;
use crate::models::projects::{PlatformEnvironmentType, ProjectRuntimeResponse};
use crate::schema::{project_runtimes, projects};
use crate::services::releases::{project_release_id_for_revision_lane, ProjectReleaseRevisionLookup};
use diesel::prelude::*;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageRuntimeDeployDenied {
    NotFound,
    NotStage,
    NoSuchRelease,
    /// More than one stage release row matches this project, session, and revision.
    AmbiguousRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageRuntimeDetachDenied {
    NotFound,
    NotStage,
    NotConnected,
}

/// IDs for NATS: the release row now live on the stage runtime and the stage release that was live before.
#[derive(Debug, Clone, Copy)]
pub struct StageDeployNatsMeta {
    pub deployed_release_id: Uuid,
    pub previous_release_id: Option<Uuid>,
}

/// Points a stage runtime at an existing stage release revision (no new `project_releases` row).
/// Used by [`crate::routes::project::handle_deploy_stage_release`] and detach-with-deploy.
pub fn apply_stage_release_deploy_to_runtime(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    runtime_id: Uuid,
    revision: f64,
    chat_session_id: Uuid,
) -> Result<Result<(ProjectRuntimeResponse, StageDeployNatsMeta), StageRuntimeDeployDenied>, diesel::result::Error> {
    let rt: Option<ProjectRuntimeResponse> = project_runtimes::table
        .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .filter(project_runtimes::project_id.eq(project_id))
        .filter(project_runtimes::id.eq(runtime_id))
        .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Stage))
        .select(ProjectRuntimeResponse::as_select())
        .first(conn)
        .optional()?;

    let Some(rt) = rt else {
        return Ok(Err(StageRuntimeDeployDenied::NotFound));
    };

    if rt.environment != PlatformEnvironmentType::Stage {
        return Ok(Err(StageRuntimeDeployDenied::NotStage));
    }

    let deployed_release_id = match project_release_id_for_revision_lane(
        conn,
        project_id,
        revision,
        true,
        chat_session_id,
    )? {
        ProjectReleaseRevisionLookup::One(id) => id,
        ProjectReleaseRevisionLookup::NoRow => {
            return Ok(Err(StageRuntimeDeployDenied::NoSuchRelease));
        }
        ProjectReleaseRevisionLookup::Ambiguous => {
            return Ok(Err(StageRuntimeDeployDenied::AmbiguousRevision));
        }
    };

    let previous_release_id = rt.live_release_id;

    let updated = diesel::update(
        project_runtimes::table
            .filter(project_runtimes::id.eq(runtime_id))
            .filter(project_runtimes::project_id.eq(project_id)),
    )
    .set(project_runtimes::live_release_id.eq(Some(deployed_release_id)))
    .returning(ProjectRuntimeResponse::as_select())
    .get_result::<ProjectRuntimeResponse>(conn)?;

    Ok(Ok((
        updated,
        StageDeployNatsMeta {
            deployed_release_id,
            previous_release_id,
        },
    )))
}

/// Clears `live_release_id` on a stage runtime (detach only).
pub fn apply_stage_runtime_detach_live_release(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    runtime_id: Uuid,
) -> Result<Result<(ProjectRuntimeResponse, Option<Uuid>), StageRuntimeDetachDenied>, diesel::result::Error> {
    let rt: Option<ProjectRuntimeResponse> = project_runtimes::table
        .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .filter(project_runtimes::project_id.eq(project_id))
        .filter(project_runtimes::id.eq(runtime_id))
        .select(ProjectRuntimeResponse::as_select())
        .first(conn)
        .optional()?;

    let Some(rt) = rt else {
        return Ok(Err(StageRuntimeDetachDenied::NotFound));
    };

    if rt.environment != PlatformEnvironmentType::Stage {
        return Ok(Err(StageRuntimeDetachDenied::NotStage));
    }

    let previous_release_id = rt.live_release_id;

    if previous_release_id.is_none() {
        return Ok(Err(StageRuntimeDetachDenied::NotConnected));
    }

    let updated = diesel::update(
        project_runtimes::table
            .filter(project_runtimes::id.eq(runtime_id))
            .filter(project_runtimes::project_id.eq(project_id)),
    )
    .set(project_runtimes::live_release_id.eq(None::<Uuid>))
    .returning(ProjectRuntimeResponse::as_select())
    .get_result::<ProjectRuntimeResponse>(conn)?;

    Ok(Ok((updated, previous_release_id)))
}
