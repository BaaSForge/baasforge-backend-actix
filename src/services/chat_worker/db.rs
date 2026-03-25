use crate::database::PooledConn;
use crate::models::projects::{ProjectReleaseResponse, ProjectUnitResponse};
use crate::schema::{project_releases, project_units};
use diesel::prelude::*;
use std::collections::HashMap;
use uuid::Uuid;

// TODO: use vector db instead of postgres
/// Load units that belong to the latest production release for the project.
/// Finds the production runtime's live_release_version, then the matching release, then units from its units_metadata.
///
/// Draft revisions are only included when they belong to `chat_session_id`, so other sessions' drafts are never exposed.
pub fn load_units_for_latest_production_release(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    query: &str,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    tracing::debug!(query = %query, "load_units_for_latest_production_release");

    // let live_version: Option<String> = project_runtimes::table
    //     .filter(project_runtimes::project_id.eq(project_id))
    //     .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Prod))
    //     .order(project_runtimes::id.asc())
    //     .select(project_runtimes::live_release_version)
    //     .first(conn)
    //     .optional()?
    //     .flatten();

    // let Some(version) = live_version else {
    //     return Ok(vec![]);
    // };

    // let units_metadata: Option<Value> = project_releases::table
    //     .filter(project_releases::project_id.eq(project_id))
    //     .filter(project_releases::version.eq(version.as_str()))
    //     .order(project_releases::created_at.desc())
    //     .select(project_releases::units_metadata)
    //     .first::<Option<Value>>(conn)
    //     .optional()?
    //     .flatten();

    // let unit_ids: Vec<Uuid> = match units_metadata {
    //     Some(Value::Object(map)) => map
    //         .keys()
    //         .filter_map(|k| Uuid::parse_str(k).ok())
    //         .collect(),
    //     Some(Value::Array(arr)) => arr
    //         .iter()
    //         .filter_map(|v| {
    //             let s = v.get("id").and_then(|v| v.as_str()).or_else(|| v.as_str())?;
    //             Uuid::parse_str(s).ok()
    //         })
    //         .collect(),
    //     _ => vec![],
    // };

    // if unit_ids.is_empty() {
    //     return Ok(vec![]);
    // }

    project_units::table
        .filter(project_units::project_id.eq(project_id))
        .filter(
            project_units::is_draft
                .eq(false)
                .or(project_units::chat_session_id.eq(Some(chat_session_id))),
        )
        // .filter(project_units::id.eq_any(&unit_ids))
        .select(ProjectUnitResponse::as_select())
        .load::<ProjectUnitResponse>(conn)
}

// Order matches `unit_ids` (omits unknown ids).
pub fn load_project_units_by_ids(
    conn: &mut PooledConn,
    project_id: Uuid,
    unit_ids: &[Uuid],
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    if unit_ids.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<ProjectUnitResponse> = project_units::table
        .filter(project_units::project_id.eq(project_id))
        .filter(project_units::id.eq_any(unit_ids))
        .select(ProjectUnitResponse::as_select())
        .load::<ProjectUnitResponse>(conn)?;
    let mut by_id: HashMap<Uuid, ProjectUnitResponse> =
        rows.into_iter().map(|u| (u.id, u)).collect();
    Ok(unit_ids
        .iter()
        .filter_map(|id| by_id.remove(id))
        .collect())
}

pub fn load_latest_releases(
    conn: &mut PooledConn,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<ProjectReleaseResponse>, diesel::result::Error> {
    let limit = limit.clamp(1, 100);
    project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .order(project_releases::created_at.desc())
        .limit(limit)
        .select(ProjectReleaseResponse::as_select())
        .load::<ProjectReleaseResponse>(conn)
}
