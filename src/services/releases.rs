//! Stage and production release creation: metadata shape and merge helpers.
use crate::models::chat_messages::strip_latest_revision_from_stored_json;
use crate::models::projects::{
    PlatformEnvironmentType, ProjectReleaseResponse, ProjectUnitResponse, ProjectUnitType,
};
use crate::schema::{project_releases, project_runtimes};
use diesel::dsl::max;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

use crate::database::PooledConn;

/// JSON shape stored in `project_releases.units_metadata` for stage/production releases built from this API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseUnitsMetadata {
    pub previous_units: Vec<ProjectUnitResponse>,
    pub new_units: Vec<ProjectUnitResponse>,
    pub combined_units: Vec<ProjectUnitResponse>,
}

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

/// Extract `combinedUnits` from release metadata (camelCase), with fallbacks for older shapes.
pub fn combined_units_from_release_metadata(
    meta: Option<&Value>,
) -> Result<Vec<ProjectUnitResponse>, serde_json::Error> {
    let Some(v) = meta else {
        return Ok(vec![]);
    };
    if let Ok(m) = serde_json::from_value::<ReleaseUnitsMetadata>(v.clone()) {
        return Ok(m.combined_units);
    }
    if let Some(cu) = v.get("combinedUnits") {
        return serde_json::from_value(cu.clone());
    }
    if let Some(cu) = v.get("combined_units") {
        return serde_json::from_value(cu.clone());
    }
    Ok(vec![])
}

pub fn release_units_metadata_to_json(
    meta: &ReleaseUnitsMetadata,
) -> Result<Value, serde_json::Error> {
    let v = serde_json::to_value(meta)?;
    Ok(strip_latest_revision_from_stored_json(v))
}

/// Resolve the baseline production release: prod runtimes' `live_release_revision` points at a
/// **production** row (`is_stage = false`). Stage vs prod is implied by [`PlatformEnvironmentType`], not a duplicate column.
pub fn load_latest_production_release_for_project(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<Option<ProjectReleaseResponse>, diesel::result::Error> {
    let revs: Vec<Option<i32>> = project_runtimes::table
        .filter(project_runtimes::project_id.eq(project_id))
        .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Prod))
        .select(project_runtimes::live_release_revision)
        .load(conn)?;

    for rev in revs.into_iter().flatten() {
        let r = project_releases::table
            .filter(project_releases::project_id.eq(project_id))
            .filter(project_releases::is_stage.eq(false))
            .filter(project_releases::revision.eq(rev))
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
pub fn next_project_release_revision(
    conn: &mut PooledConn,
    project_id: Uuid,
    is_stage: bool,
) -> Result<i32, diesel::result::Error> {
    let m: Option<i32> = project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::is_stage.eq(is_stage))
        .select(max(project_releases::revision))
        .first(conn)?;
    Ok(m.unwrap_or(0) + 1)
}
