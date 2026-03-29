//! Shared DB logic for pointing a stage project runtime at a stage release revision, or clearing it.

use crate::database::PooledConn;
use crate::models::projects::{PlatformEnvironmentType, ProjectRuntimeResponse};
use crate::schema::{project_releases, project_runtimes, projects};
use diesel::prelude::*;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageRuntimeDeployDenied {
    NotFound,
    NotStage,
    NoSuchRelease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageRuntimeDetachDenied {
    NotFound,
    NotStage,
    NotConnected,
}

/// Points a stage runtime at an existing stage release revision (no new `project_releases` row).
/// Used by [`crate::routes::project::handle_deploy_stage_release`] and detach-with-deploy.
pub fn apply_stage_release_deploy_to_runtime(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    runtime_id: Uuid,
    revision: i32,
) -> Result<Result<ProjectRuntimeResponse, StageRuntimeDeployDenied>, diesel::result::Error> {
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

    let release_ok = project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::revision.eq(revision))
        .filter(project_releases::is_stage.eq(true))
        .select(project_releases::id)
        .first::<Uuid>(conn)
        .optional()?
        .is_some();

    if !release_ok {
        return Ok(Err(StageRuntimeDeployDenied::NoSuchRelease));
    }

    let updated = diesel::update(
        project_runtimes::table
            .filter(project_runtimes::id.eq(runtime_id))
            .filter(project_runtimes::project_id.eq(project_id)),
    )
    .set(project_runtimes::live_release_revision.eq(Some(revision)))
    .returning(ProjectRuntimeResponse::as_select())
    .get_result::<ProjectRuntimeResponse>(conn)?;

    Ok(Ok(updated))
}

/// Clears `live_release_revision` on a stage runtime (detach only).
pub fn apply_stage_runtime_detach_live_release(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    runtime_id: Uuid,
) -> Result<Result<ProjectRuntimeResponse, StageRuntimeDetachDenied>, diesel::result::Error> {
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

    if rt.live_release_revision.is_none() {
        return Ok(Err(StageRuntimeDetachDenied::NotConnected));
    }

    let updated = diesel::update(
        project_runtimes::table
            .filter(project_runtimes::id.eq(runtime_id))
            .filter(project_runtimes::project_id.eq(project_id)),
    )
    .set(project_runtimes::live_release_revision.eq(None::<i32>))
    .returning(ProjectRuntimeResponse::as_select())
    .get_result::<ProjectRuntimeResponse>(conn)?;

    Ok(Ok(updated))
}
