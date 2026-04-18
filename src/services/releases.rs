//! Stage and production release creation: metadata shape and merge helpers.
//!
//! Production releases are created by promoting an existing stage row: [`insert_production_release_from_stage_row`]
//! copies `units_metadata` (and session linkage) from that row. Unit diffs and merges run only when
//! the stage release is created—not again at production time.
use crate::database::PooledConn;
use crate::models::chat_messages::{ChatMessagePayload, ChatMessageResponse, ChatMessageType};
use crate::models::projects::{
    PlatformEnvironmentType, ProjectReleaseResponse, ProjectReleaseWithBaseline, ProjectUnitResponse,
    ProjectUnitType,
};
use crate::schema::{chat_messages, chat_sessions, project_releases, project_runtimes};
use crate::services::chat_worker::db::load_project_units_by_keys;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_types::Integer;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub use crate::services::chat_worker::db::load_latest_production_release_for_project;

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
/// larger `revision`, or on a tie prefer the tip when it comes from a different chat session than
/// the baseline row ([`session_unit_tip_replaces_previous`]).
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
                if session_unit_tip_replaces_previous(prev, u) {
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
                Some(p) => session_unit_tip_replaces_previous(p, u),
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

struct ReleaseUnitDiff {
    prev_count: usize,
    comb_count: usize,
    new_keys: Vec<LogicalUnitKey>,
    deleted_keys: Vec<LogicalUnitKey>,
    /// Current rows for logically modified units, sorted by path.
    modified_current: Vec<ProjectUnitResponse>,
}

fn release_unit_diff(
    previous_combined: &[ProjectUnitResponse],
    combined_units: &[ProjectUnitResponse],
) -> ReleaseUnitDiff {
    fn active_map(units: &[ProjectUnitResponse]) -> HashMap<LogicalUnitKey, &ProjectUnitResponse> {
        units
            .iter()
            .filter(|u| !u.is_deleted)
            .map(|u| (logical_unit_key(u), u))
            .collect()
    }

    let prev = active_map(previous_combined);
    let comb = active_map(combined_units);

    let prev_count = prev.len();
    let comb_count = comb.len();
    let mut new_keys: Vec<LogicalUnitKey> = comb
        .keys()
        .filter(|k| !prev.contains_key(*k))
        .cloned()
        .collect();
    let mut deleted_keys: Vec<LogicalUnitKey> = prev
        .keys()
        .filter(|k| !comb.contains_key(*k))
        .cloned()
        .collect();
    let mut modified_current: Vec<ProjectUnitResponse> = Vec::new();
    for k in comb.keys() {
        if let (Some(pb), Some(cb)) = (prev.get(k), comb.get(k)) {
            if pb.id != cb.id {
                modified_current.push((*cb).clone());
            }
        }
    }

    new_keys.sort_by(|a, b| logical_unit_sort_tuple(a).cmp(&logical_unit_sort_tuple(b)));
    deleted_keys.sort_by(|a, b| logical_unit_sort_tuple(a).cmp(&logical_unit_sort_tuple(b)));
    modified_current.sort_by(|a, b| {
        logical_unit_sort_tuple(&logical_unit_key(a)).cmp(&logical_unit_sort_tuple(&logical_unit_key(b)))
    });

    ReleaseUnitDiff {
        prev_count,
        comb_count,
        new_keys,
        deleted_keys,
        modified_current,
    }
}

/// Ultra-short Markdown (about three lines, ~20 words) summarizing the same diff as [`release_notes_markdown`].
#[must_use]
pub fn release_brief_note_markdown(
    previous_combined: &[ProjectUnitResponse],
    combined_units: &[ProjectUnitResponse],
) -> String {
    let d = release_unit_diff(previous_combined, combined_units);
    let n_new = d.new_keys.len();
    let n_deleted = d.deleted_keys.len();
    let n_modified = d.modified_current.len();
    let total_changes = n_new + n_deleted + n_modified;
    let tail = if total_changes == 0 {
        "**No** unit changes.".to_string()
    } else {
        format!("**{}** total changes.", total_changes)
    };
    format!(
        "Previously **{}** active units → **{}** now.\n**+{}** new · **{}** modified · **{}** removed.\n{}",
        d.prev_count,
        d.comb_count,
        n_new,
        n_modified,
        n_deleted,
        tail
    )
}

/// Markdown release notes comparing the previous combined snapshot to this release’s combined snapshot.
/// Counts **active** units only (`is_deleted == false`). Baseline size is the number of active units
/// before this release; total changes is new + deleted + modified.
pub fn release_notes_markdown(
    previous_combined: &[ProjectUnitResponse],
    combined_units: &[ProjectUnitResponse],
) -> String {
    let d = release_unit_diff(previous_combined, combined_units);
    fn active_map(units: &[ProjectUnitResponse]) -> HashMap<LogicalUnitKey, &ProjectUnitResponse> {
        units
            .iter()
            .filter(|u| !u.is_deleted)
            .map(|u| (logical_unit_key(u), u))
            .collect()
    }
    let prev = active_map(previous_combined);
    let comb = active_map(combined_units);

    let n_new = d.new_keys.len();
    let n_deleted = d.deleted_keys.len();
    let n_modified = d.modified_current.len();
    let total_changes = n_new + n_deleted + n_modified;

    let mut out = String::new();
    out.push_str("## Summary\n\n");
    out.push_str(&format!(
        "- Active units in the previous snapshot (baseline): **{}**\n",
        d.prev_count
    ));
    out.push_str(&format!("- New units: **{}**\n", n_new));
    out.push_str(&format!("- Deleted units: **{}**\n", n_deleted));
    out.push_str(&format!("- Modified units: **{}**\n", n_modified));
    out.push_str(&format!("- Total changes: **{}**\n\n", total_changes));

    out.push_str("## New\n\n");
    if d.new_keys.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        for k in &d.new_keys {
            let u = comb[k];
            out.push_str(&format!(
                "- `{}` ({}) — created\n",
                u.id,
                u.unit_type.as_slug()
            ));
        }
        out.push('\n');
    }

    out.push_str("## Deleted\n\n");
    if d.deleted_keys.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        for k in &d.deleted_keys {
            let u = prev[k];
            out.push_str(&format!(
                "- `{}` ({}) — removed\n",
                u.id,
                u.unit_type.as_slug()
            ));
        }
        out.push('\n');
    }

    out.push_str("## Modified\n\n");
    if d.modified_current.is_empty() {
        out.push_str("_None._\n");
    } else {
        for cur in &d.modified_current {
            out.push_str(&format!(
                "- `{}` ({}) — new revision **{}**\n",
                cur.id,
                cur.unit_type.as_slug(),
                cur.revision
            ));
        }
    }

    out
}

fn logical_unit_sort_tuple(k: &LogicalUnitKey) -> (&str, &str, &'static str, &str) {
    (
        k.0.as_str(),
        k.1.as_str(),
        k.2.as_slug(),
        k.3.as_str(),
    )
}

/// Result of resolving `project_releases.id` by `(project_id, chat_session_id, revision, lane)` — see
/// [`project_release_id_for_revision_lane`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectReleaseRevisionLookup {
    NoRow,
    One(Uuid),
    /// More than one row matches (should not happen for a normal DB; indicates inconsistent data).
    Ambiguous,
}

/// Looks up `project_releases.id` for this project, revision, stage vs production lane, **and**
/// chat session. Revision floats are not unique per project; callers must always pass the session
/// that owns the release row.
pub fn project_release_id_for_revision_lane(
    conn: &mut PooledConn,
    project_id: Uuid,
    revision: f64,
    is_stage: bool,
    chat_session_id: Uuid,
) -> Result<ProjectReleaseRevisionLookup, diesel::result::Error> {
    let ids: Vec<Uuid> = project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::revision.eq(revision))
        .filter(project_releases::is_stage.eq(is_stage))
        .filter(project_releases::chat_session_id.eq(Some(chat_session_id)))
        .select(project_releases::id)
        .load(conn)?;
    Ok(match ids.len() {
        0 => ProjectReleaseRevisionLookup::NoRow,
        1 => ProjectReleaseRevisionLookup::One(ids[0]),
        _ => ProjectReleaseRevisionLookup::Ambiguous,
    })
}

/// When `prev` and `tip` share the same [`LogicalUnitKey`] and revision number, prefer `tip` if it
/// belongs to a different chat session (reused revision across sessions).
#[must_use]
pub fn session_unit_tip_replaces_previous(
    prev: &ProjectUnitResponse,
    tip: &ProjectUnitResponse,
) -> bool {
    if tip.revision > prev.revision {
        return true;
    }
    if tip.revision < prev.revision {
        return false;
    }
    prev.chat_session_id != tip.chat_session_id
}

/// Production runtime’s current live release id and the `chat_session_id` stored on that release row
/// (if any). Used with the session’s baseline snapshot to detect a stale base for UI / LLM hints.
pub fn prod_live_release_source_chat_session(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<(Option<Uuid>, Option<Uuid>), diesel::result::Error> {
    let current = live_release_id_from_production_runtime(conn, project_id)?;
    let source_session = if let Some(release_id) = current {
        project_releases::table
            .filter(project_releases::id.eq(release_id))
            .filter(project_releases::project_id.eq(project_id))
            .select(project_releases::chat_session_id)
            .first::<Option<Uuid>>(conn)?
    } else {
        None
    };
    Ok((current, source_session))
}

/// True when live production moved away from the snapshot taken at session creation **and** the
/// current live production release was not produced from this session—used for UI and LLM warnings
/// (staging/deploy and unit edits are still allowed; releases from this chat may revert newer prod).
pub fn stale_base_release_from_baseline(
    session_baseline_live_release_id: Option<Uuid>,
    current_live_release_id: Option<Uuid>,
    current_live_release_chat_session_id: Option<Uuid>,
    this_session_id: Uuid,
) -> bool {
    if current_live_release_id == session_baseline_live_release_id {
        return false;
    }
    if current_live_release_id.is_none() {
        return false;
    }
    current_live_release_chat_session_id != Some(this_session_id)
}

pub fn chat_session_stale_base_release(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<bool, diesel::result::Error> {
    let snapshot: Option<Uuid> = match chat_sessions::table
        .filter(chat_sessions::id.eq(session_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .select(chat_sessions::live_production_release_id)
        .first::<Option<Uuid>>(conn)
        .optional()?
    {
        Some(s) => s,
        None => return Ok(false),
    };
    let (current, source) = prod_live_release_source_chat_session(conn, project_id)?;
    Ok(stale_base_release_from_baseline(
        snapshot,
        current,
        source,
        session_id,
    ))
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

/// Session baseline: `chat_sessions.live_production_release_id` and the loaded **production** row, if it still exists.
/// All stage and production releases created in that session share this base for revision major and stage unit merges.
///
/// Returns `(None, None)` when the session has no baseline. If the session references an id that no longer exists,
/// returns `(None, None)` for safe FK handling; revision major falls back as if there were no prior prod release.
pub fn load_baseline_production_release_for_chat_session(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<(Option<Uuid>, Option<ProjectReleaseResponse>), diesel::result::Error> {
    let baseline_id: Option<Uuid> = chat_sessions::table
        .filter(chat_sessions::id.eq(session_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .select(chat_sessions::live_production_release_id)
        .first::<Option<Uuid>>(conn)?;
    let Some(id) = baseline_id else {
        return Ok((None, None));
    };
    let row = project_releases::table
        .filter(project_releases::id.eq(id))
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::is_stage.eq(false))
        .select(ProjectReleaseResponse::as_select())
        .first(conn)
        .optional()?;
    if row.is_none() {
        return Ok((None, None));
    }
    Ok((Some(id), row))
}

/// Resolve `base_production_release` for each release (from `base_production_release_id`) for JSON APIs.
pub fn attach_base_production_releases(
    conn: &mut PooledConn,
    project_id: Uuid,
    releases: Vec<ProjectReleaseResponse>,
) -> Result<Vec<ProjectReleaseWithBaseline>, diesel::result::Error> {
    let ids: Vec<Uuid> = releases
        .iter()
        .filter_map(|r| r.base_production_release_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(releases
            .into_iter()
            .map(|release| ProjectReleaseWithBaseline {
                release,
                base_production_release: None,
            })
            .collect());
    }
    let base_rows: Vec<ProjectReleaseResponse> = project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::id.eq_any(&ids))
        .select(ProjectReleaseResponse::as_select())
        .load(conn)?;
    let map: HashMap<Uuid, ProjectReleaseResponse> =
        base_rows.into_iter().map(|r| (r.id, r)).collect();
    Ok(releases
        .into_iter()
        .map(|release| {
            let base = release
                .base_production_release_id
                .and_then(|id| map.get(&id).cloned());
            ProjectReleaseWithBaseline {
                release,
                base_production_release: base,
            }
        })
        .collect())
}

/// [`HashMap`] from [`attach_base_production_releases`] for [`crate::services::chat_worker::db::load_owned_releases_by_chat_session`].
pub fn enrich_owned_releases_by_session_map(
    conn: &mut PooledConn,
    project_id: Uuid,
    map: HashMap<Uuid, Vec<ProjectReleaseResponse>>,
) -> Result<HashMap<Uuid, Vec<ProjectReleaseWithBaseline>>, diesel::result::Error> {
    let flat: Vec<ProjectReleaseResponse> = map.values().flatten().cloned().collect();
    if flat.is_empty() {
        return Ok(map.into_keys().map(|k| (k, Vec::new()))
            .collect());
    }
    let enriched = attach_base_production_releases(conn, project_id, flat)?;
    let by_id: HashMap<Uuid, ProjectReleaseWithBaseline> =
        enriched.into_iter().map(|e| (e.release.id, e)).collect();
    Ok(map
        .into_iter()
        .map(|(sid, vec)| {
            let v = vec
                .into_iter()
                .map(|r| {
                    by_id
                        .get(&r.id)
                        .cloned()
                        .unwrap_or(ProjectReleaseWithBaseline {
                            release: r,
                            base_production_release: None,
                        })
                })
                .collect();
            (sid, v)
        })
        .collect())
}

/// Fills `base_production_release` on release-controller message payloads for clients.
pub fn enrich_chat_message_response_releases(
    conn: &mut PooledConn,
    project_id: Uuid,
    mut msg: ChatMessageResponse,
) -> Result<ChatMessageResponse, diesel::result::Error> {
    let Some(payload) = msg.payload.take() else {
        return Ok(msg);
    };
    let payload = match payload {
        ChatMessagePayload::ReleaseControllerBox(mut p) => {
            let mut batch = Vec::new();
            if let Some(ref s) = p.stage_release {
                batch.push(s.release.clone());
            }
            if let Some(ref pr) = p.prod_release {
                batch.push(pr.release.clone());
            }
            if !batch.is_empty() {
                let enriched = attach_base_production_releases(conn, project_id, batch)?;
                let by_id: HashMap<Uuid, ProjectReleaseWithBaseline> =
                    enriched.into_iter().map(|e| (e.release.id, e)).collect();
                if let Some(s) = p.stage_release.as_ref() {
                    p.stage_release = by_id.get(&s.release.id).cloned();
                }
                if let Some(pr) = p.prod_release.as_ref() {
                    p.prod_release = by_id.get(&pr.release.id).cloned();
                }
            }
            ChatMessagePayload::ReleaseControllerBox(p)
        }
        other => other,
    };
    msg.payload = Some(payload);
    Ok(msg)
}

/// `(chat_session_id, unit_id)` refs present in the latest production release combined snapshot.
pub fn released_unit_refs_set_from_latest_production(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<HashSet<(Uuid, String)>, diesel::result::Error> {
    Ok(
        load_latest_production_release_for_project(conn, project_id)?
            .and_then(|r| r.units_metadata)
            .map(|m| {
                m.combined_unit_refs
                    .into_iter()
                    .map(|r| (r.chat_session_id, r.unit_id))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Sets [`ProjectUnitResponse::is_draft`] per unit: `false` if the ref appears in the latest
/// production combined metadata, otherwise `true`.
pub fn attach_is_draft_flags_from_latest_production(
    conn: &mut PooledConn,
    project_id: Uuid,
    mut units: Vec<ProjectUnitResponse>,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let released = released_unit_refs_set_from_latest_production(conn, project_id)?;
    for u in units.iter_mut() {
        u.is_draft = Some(!released.contains(&(u.chat_session_id, u.id.clone())));
    }
    Ok(units)
}

/// Chat “current units” list: session baseline production snapshot merged with this session’s unit tips.
/// Uses the release that was live when the session was created so the result reflects only what
/// was deployed at session-open time plus the session’s own drafts.
pub fn load_chat_session_units_merged_with_latest_production(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let previous_combined = {
        let (_baseline_id, baseline_release) =
            load_baseline_production_release_for_chat_session(conn, project_id, session_id)?;
        match baseline_release.as_ref().and_then(|r| r.units_metadata.as_ref()) {
            Some(m) => combined_units_from_release_metadata(conn, project_id, Some(m))?,
            None => Vec::new(),
        }
    };
    let session_tips = crate::services::chat_worker::db::load_chat_session_unit_tips_for_merge(
        conn,
        project_id,
        session_id,
    )?;
    Ok(merge_combined_units(&previous_combined, &session_tips))
}

/// 1-based index of this message among `ReleaseControllerBox` rows in the session (order: `created_at`, `id`).
pub fn release_controller_ordinal_in_session(
    conn: &mut PooledConn,
    session_id: Uuid,
    message_id: Uuid,
) -> Result<i64, diesel::result::Error> {
    let target = chat_messages::table
        .filter(chat_messages::id.eq(message_id))
        .filter(chat_messages::chat_session_id.eq(session_id))
        .select((chat_messages::created_at, chat_messages::id))
        .first::<(DateTime<Utc>, Uuid)>(conn)?;

    chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::type_.eq(ChatMessageType::ReleaseControllerBox))
        .filter(
            chat_messages::created_at.lt(target.0).or(
                chat_messages::created_at
                    .eq(target.0)
                    .and(chat_messages::id.le(message_id)),
            ),
        )
        .count()
        .get_result(conn)
}

#[inline]
fn compose_chat_release_revision(major: i64, ordinal: i64) -> f64 {
    major as f64 + ordinal as f64 / 10.0
}

/// Revision for a release created from a release-controller message: `major + ordinal/10`, with
/// `major = floor(session baseline prod revision) + 1` (or `1` if none), and `ordinal` the 1-based index among
/// [`ChatMessageType::ReleaseControllerBox`] rows in the session (same rule for stage and production).
///
/// Uses the chat session’s baseline production release (see [`load_baseline_production_release_for_chat_session`]),
/// i.e. the live production release when the session was created—so multiple controller rows in the same chat share one major.
///
/// Call only inside an **outer** database transaction. Takes the same lane advisory lock as the old
/// `MAX(revision)+1` path so concurrent in-chat creates serialize per project and lane.
pub fn revision_for_chat_release_controller(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    is_stage: bool,
) -> Result<f64, diesel::result::Error> {
    let (k1, k2) = project_release_lane_advisory_lock_keys(project_id, is_stage);
    diesel::sql_query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind::<Integer, _>(k1)
        .bind::<Integer, _>(k2)
        .execute(conn)?;

    let (_baseline_id, baseline_row) =
        load_baseline_production_release_for_chat_session(conn, project_id, session_id)?;
    let base_floor = baseline_row.map(|r| r.revision.floor()).unwrap_or(0.0);
    let major = (base_floor as i64).saturating_add(1);

    let ordinal = release_controller_ordinal_in_session(conn, session_id, message_id)?;
    Ok(compose_chat_release_revision(major, ordinal))
}

/// `true` when the stage row is scoped to `session_id` (`project_releases.chat_session_id`).
#[must_use]
pub fn stage_release_belongs_to_chat_session(
    stage: &ProjectReleaseResponse,
    session_id: Uuid,
) -> bool {
    stage.chat_session_id == Some(session_id)
}

/// Inserts a **production** `project_releases` row by copying persisted content from a **stage** row.
///
/// Callers must load `stage` from the database (`is_stage == true`, matching project/session) and
/// pass the same `stage_release_id` the client sent. This function assigns a new production
/// [`revision_for_chat_release_controller`] (production lane); it does **not** recompute
/// [`ReleaseUnitsMetadata`]—that was fixed when the stage release was created.
///
/// **Invariant:** `stage` must belong to `session_id` (see [`stage_release_belongs_to_chat_session`]);
/// the HTTP handler loads the row with `chat_session_id = session_id`.
pub fn insert_production_release_from_stage_row(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    stage: &ProjectReleaseResponse,
) -> Result<ProjectReleaseResponse, diesel::result::Error> {
    debug_assert!(
        stage_release_belongs_to_chat_session(stage, session_id),
        "production promotion requires a stage row scoped to this chat session"
    );
    let revision = revision_for_chat_release_controller(conn, project_id, session_id, message_id, false)?;
    diesel::insert_into(project_releases::table)
        .values((
            project_releases::project_id.eq(project_id),
            project_releases::revision.eq(revision),
            project_releases::units_metadata.eq(stage.units_metadata.clone()),
            project_releases::notes.eq(stage.notes.clone()),
            project_releases::brief_note.eq(stage.brief_note.clone()),
            project_releases::chat_session_id.eq(stage.chat_session_id),
            project_releases::is_stage.eq(false),
            project_releases::base_production_release_id.eq(stage.base_production_release_id),
        ))
        .returning(ProjectReleaseResponse::as_select())
        .get_result(conn)
}
