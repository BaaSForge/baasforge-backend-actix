//! Stage and production release creation: metadata shape and merge helpers.
use crate::database::PooledConn;
use crate::models::projects::{
    PlatformEnvironmentType, ProjectReleaseResponse, ProjectUnitResponse, ProjectUnitType,
};
use crate::schema::{project_releases, project_runtimes};
use crate::services::chat_worker::db::load_project_units_by_keys;
use diesel::dsl::max;
use diesel::prelude::*;
use diesel::sql_types::Integer;
use std::collections::HashMap;
use uuid::Uuid;

/// Two `int4` keys for [`pg_advisory_xact_lock`](https://www.postgresql.org/docs/current/functions-admin.html#FUNCTIONS-ADVISORY-LOCKS).
/// Stage and production lanes for the same project must not share a lock (they have independent revision sequences).
fn project_release_lane_advisory_lock_keys(project_id: Uuid, is_stage: bool) -> (i32, i32) {
    let b = project_id.as_bytes();
    let k1 = i32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let mut k2 = i32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    if !is_stage {
        k2 ^= 0x6A09_E667;
    }
    (k1, k2)
}

pub use crate::models::projects::{ReleaseUnitRef, ReleaseUnitsMetadata};

pub type LogicalUnitKey = (String, String, ProjectUnitType, String);

#[inline]
pub fn logical_unit_key(u: &ProjectUnitResponse) -> LogicalUnitKey {
    (
        u.context_slug.clone(),
        u.module_slug.clone(),
        u.unit_type,
        u.unit_name_slug.clone(),
    )
}

/// Overlay session tips onto the previous production snapshot: per logical unit, keep the row with the
/// larger `revision` (session wins when it introduces a newer revision).
pub fn merge_combined_units(
    previous_combined: &[ProjectUnitResponse],
    session_tips: &[ProjectUnitResponse],
) -> Vec<ProjectUnitResponse> {
    let mut map: HashMap<LogicalUnitKey, ProjectUnitResponse> = HashMap::new();
    for u in previous_combined {
        map.insert(logical_unit_key(u), u.clone());
    }
    for u in session_tips {
        let k = logical_unit_key(u);
        match map.get(&k) {
            None => {
                map.insert(k, u.clone());
            }
            Some(prev) => {
                if u.revision > prev.revision {
                    map.insert(k, u.clone());
                }
            }
        }
    }
    let mut out: Vec<_> = map.into_values().collect();
    out.sort_by(|a, b| {
        a.context_slug
            .cmp(&b.context_slug)
            .then_with(|| a.module_slug.cmp(&b.module_slug))
            .then_with(|| a.unit_name_slug.cmp(&b.unit_name_slug))
    });
    out
}

/// Units introduced in this session relative to the previous production `combined` snapshot: new logical
/// units, or strictly newer `revision` for an existing path.
pub fn compute_new_units(
    previous_combined: &[ProjectUnitResponse],
    session_tips: &[ProjectUnitResponse],
) -> Vec<ProjectUnitResponse> {
    let prev_map: HashMap<LogicalUnitKey, &ProjectUnitResponse> = previous_combined
        .iter()
        .map(|u| (logical_unit_key(u), u))
        .collect();
    session_tips
        .iter()
        .filter(|u| {
            match prev_map.get(&logical_unit_key(u)) {
                None => true,
                Some(p) => u.revision > p.revision,
            }
        })
        .cloned()
        .collect()
}

fn combined_rows_from_unit_refs(
    conn: &mut PooledConn,
    project_id: Uuid,
    refs: &[ReleaseUnitRef],
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let keys: Vec<(Uuid, String)> = refs
        .iter()
        .map(|r| (r.chat_session_id, r.unit_id.clone()))
        .collect();
    load_project_units_by_keys(conn, project_id, &keys)
}

/// Loads the combined snapshot from [`ReleaseUnitsMetadata`] stored in `units_metadata`.
pub fn combined_units_from_release_metadata(
    conn: &mut PooledConn,
    project_id: Uuid,
    meta: Option<&ReleaseUnitsMetadata>,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let Some(m) = meta else {
        return Ok(vec![]);
    };
    combined_rows_from_unit_refs(conn, project_id, &m.combined_unit_refs)
}

pub fn project_unit_refs_from_responses(units: &[ProjectUnitResponse]) -> Vec<ReleaseUnitRef> {
    units
        .iter()
        .map(|u| ReleaseUnitRef {
            chat_session_id: u.chat_session_id,
            unit_id: u.id.clone(),
        })
        .collect()
}

/// Looks up `project_releases.id` for this project, revision, and stage vs production lane
/// (used when the API still selects a release by revision).
pub fn project_release_id_for_revision_lane(
    conn: &mut PooledConn,
    project_id: Uuid,
    revision: f64,
    is_stage: bool,
) -> Result<Option<Uuid>, diesel::result::Error> {
    project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::revision.eq(revision))
        .filter(project_releases::is_stage.eq(is_stage))
        .select(project_releases::id)
        .first::<Uuid>(conn)
        .optional()
}

/// `live_release_id` from the project’s production runtime (`environment = Prod`), if that runtime
/// exists and has a live release attached.
pub fn live_release_id_from_production_runtime(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<Option<Uuid>, diesel::result::Error> {
    let row = project_runtimes::table
        .filter(project_runtimes::project_id.eq(project_id))
        .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Prod))
        .select(project_runtimes::live_release_id)
        .first::<Option<Uuid>>(conn)
        .optional()?;
    Ok(row.flatten())
}

/// Production release id currently stored on prod runtimes’ `live_release_id`, if any.
pub fn production_release_id_for_live_runtimes(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<Option<Uuid>, diesel::result::Error> {
    let ids: Vec<Option<Uuid>> = project_runtimes::table
        .filter(project_runtimes::project_id.eq(project_id))
        .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Prod))
        .select(project_runtimes::live_release_id)
        .load(conn)?;

    Ok(ids.into_iter().flatten().next())
}

/// Resolve the baseline production release: prod runtimes’ `live_release_id` points at a
/// **production** row (`is_stage = false`), or fall back to the latest prod release by `created_at`.
pub fn load_latest_production_release_for_project(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<Option<ProjectReleaseResponse>, diesel::result::Error> {
    let ids: Vec<Option<Uuid>> = project_runtimes::table
        .filter(project_runtimes::project_id.eq(project_id))
        .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Prod))
        .select(project_runtimes::live_release_id)
        .load(conn)?;

    for id in ids.into_iter().flatten() {
        let r = project_releases::table
            .filter(project_releases::id.eq(id))
            .filter(project_releases::project_id.eq(project_id))
            .filter(project_releases::is_stage.eq(false))
            .select(ProjectReleaseResponse::as_select())
            .first(conn)
            .optional()?;
        if r.is_some() {
            return Ok(r);
        }
    }

    project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::is_stage.eq(false))
        .order(project_releases::created_at.desc())
        .select(ProjectReleaseResponse::as_select())
        .first(conn)
        .optional()
}

/// Next `revision` for a new row with this `is_stage` flag (`MAX(revision) + 1` within project + stage/prod lane).
///
/// Call only inside an **outer** database transaction. Takes a transaction-scoped advisory lock so
/// concurrent callers (e.g. multiple chat sessions) cannot read the same `MAX` and insert duplicate revisions.
pub fn next_project_release_revision(
    conn: &mut PooledConn,
    project_id: Uuid,
    is_stage: bool,
) -> Result<f64, diesel::result::Error> {
    let (k1, k2) = project_release_lane_advisory_lock_keys(project_id, is_stage);
    diesel::sql_query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind::<Integer, _>(k1)
        .bind::<Integer, _>(k2)
        .execute(conn)?;
    let m: Option<f64> = project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::is_stage.eq(is_stage))
        .select(max(project_releases::revision))
        .first(conn)?;
    Ok(m.unwrap_or(0.0) + 1.0)
}
