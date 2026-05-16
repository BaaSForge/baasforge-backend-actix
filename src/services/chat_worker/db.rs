//! PostgreSQL access for chat-time project units and related helpers.
//!
//! **Unit rows:** The authoritative store for unit definitions is the `project_units` table. Load
//! rows only via queries on that table (typically `project_id` + `chat_session_id` + full unit `id`).
//!
//! **Release metadata is not a loader for chat tools:** `project_releases.units_metadata` / combined
//! unit refs are a persisted snapshot for release workflows, not a source to resolve which units to
//! load for features that run inside a chat session. For example, `finish_observability_analysis` and
//! `finish_architecture_analysis` merge `related_units` via [`load_related_units_for_chat_session_by_full_ids`]
//! (same effective set as [`load_units_for_latest_production_release`]), because baseline rows may
//! use another `chat_session_id` than the invoking session. Functions that hydrate from release
//! metadata ([`load_units_from_latest_production_metadata`], [`load_units_from_release_metadata`])
//! exist for release-specific code paths; do not use a release row to populate session-scoped tool
//! payloads in new code.
use crate::database::PooledConn;
use crate::libs::unit_slug::{UnitPath, compose_unit_id, parse_unit_path};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::models::projects::{
    PlatformEnvironmentType, ProjectReleaseResponse, ProjectUnitResponse, ProjectUnitRow,
    ProjectUnitType,
};
use crate::models::runtime_env_vars::{ProjectRuntimeEnvVarRow, RuntimeEnvKeySummary};
use crate::schema::{
    chat_messages, chat_sessions, project_releases, project_runtime_env_vars, project_runtimes,
    project_units, projects,
};
use chrono::{DateTime, Utc};
use diesel::OptionalExtension;
use diesel::QueryableByName;
use diesel::dsl::{exists, max, not};
use diesel::prelude::*;
use diesel::sql_types;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Returned inside [`Result::Ok`] from [`load_chat_session_draft_units_as_of_release_controller_message`]
/// when the message id does not denote a release-controller row for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftUnitsAsOfReleaseControllerError {
    MessageNotFound,
    NotReleaseControllerMessage,
}

pub enum LastReleaseControllerNotDisabledLookup {
    SessionNotFound,
    NoEligibleMessage,
    Found(ChatMessageRow),
}

pub fn load_last_not_disabled_release_controller_for_session(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<LastReleaseControllerNotDisabledLookup, diesel::result::Error> {
    let session_ok: Option<Uuid> = chat_sessions::table
        .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .filter(chat_sessions::id.eq(session_id))
        .select(chat_sessions::id)
        .first(conn)
        .optional()?;
    if session_ok.is_none() {
        return Ok(LastReleaseControllerNotDisabledLookup::SessionNotFound);
    }

    let rows: Vec<ChatMessageRow> = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(chat_messages::type_.eq(ChatMessageType::ReleaseControllerBox))
        .order((chat_messages::created_at.desc(), chat_messages::id.desc()))
        .select(ChatMessageRow::as_select())
        .load(conn)?;

    for row in rows {
        let Ok(Some(ChatMessagePayload::ReleaseControllerBox(p))) =
            ChatMessagePayload::from_value(&row.type_, row.payload.clone())
        else {
            continue;
        };
        if !p.is_disabled_due_to_reject {
            return Ok(LastReleaseControllerNotDisabledLookup::Found(row));
        }
    }

    Ok(LastReleaseControllerNotDisabledLookup::NoEligibleMessage)
}

type LogicalUnitKey = (String, String, ProjectUnitType, String);

/// Columns for [`ProjectUnitRow`] when selecting from a `project_units` alias (order matches the struct).
macro_rules! select_project_unit_row {
    ($a:expr) => {
        (
            $a.field(project_units::project_id),
            $a.field(project_units::chat_session_id),
            $a.field(project_units::id),
            $a.field(project_units::revision),
            $a.field(project_units::code),
            $a.field(project_units::compiled_code),
            $a.field(project_units::explanation),
            $a.field(project_units::unit_type),
            $a.field(project_units::is_deleted),
            $a.field(project_units::context_slug),
            $a.field(project_units::module_slug),
            $a.field(project_units::unit_name_slug),
            $a.field(project_units::metadata),
            $a.field(project_units::depends_on),
        )
    };
}

/// For each logical unit `(chat_session_id, context_slug, module_slug, unit_type, unit_name_slug)` under
/// `project_id`, compute `MAX(revision)` and attach it as `latest_revision` on each row.
pub fn project_units_with_latest_revision(
    conn: &mut PooledConn,
    project_id: Uuid,
    rows: Vec<ProjectUnitRow>,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    if rows.is_empty() {
        return Ok(vec![]);
    }

    let aggregates: Vec<(Uuid, String, String, ProjectUnitType, String, Option<i32>)> =
        project_units::table
            .filter(project_units::project_id.eq(project_id))
            .group_by((
                project_units::chat_session_id,
                project_units::context_slug,
                project_units::module_slug,
                project_units::unit_type,
                project_units::unit_name_slug,
            ))
            .select((
                project_units::chat_session_id,
                project_units::context_slug,
                project_units::module_slug,
                project_units::unit_type,
                project_units::unit_name_slug,
                max(project_units::revision),
            ))
            .load(conn)?;

    let mut map: HashMap<(Uuid, String, String, ProjectUnitType, String), i32> = HashMap::new();
    for (sid, ctx, module, utype, unit_name, max_rev) in aggregates {
        if let Some(m) = max_rev {
            map.insert((sid, ctx, module, utype, unit_name), m);
        }
    }

    Ok(rows
        .into_iter()
        .map(|row| {
            let key = (
                row.chat_session_id,
                row.context_slug.clone(),
                row.module_slug.clone(),
                row.unit_type,
                row.unit_name_slug.clone(),
            );
            let latest = map.get(&key).copied().unwrap_or(row.revision);
            ProjectUnitResponse::from_row(row, latest)
        })
        .collect())
}

/// `inner` is the same logical unit as `outer` and has a strictly larger `revision`.
macro_rules! inner_is_newer_same_unit {
    ($outer:expr, $inner:expr) => {
        $inner
            .field(project_units::project_id)
            .eq($outer.field(project_units::project_id))
            .and(
                $inner
                    .field(project_units::chat_session_id)
                    .eq($outer.field(project_units::chat_session_id)),
            )
            .and(
                $inner
                    .field(project_units::context_slug)
                    .eq($outer.field(project_units::context_slug)),
            )
            .and(
                $inner
                    .field(project_units::module_slug)
                    .eq($outer.field(project_units::module_slug)),
            )
            .and(
                $inner
                    .field(project_units::unit_type)
                    .eq($outer.field(project_units::unit_type)),
            )
            .and(
                $inner
                    .field(project_units::unit_name_slug)
                    .eq($outer.field(project_units::unit_name_slug)),
            )
            .and(
                $inner
                    .field(project_units::revision)
                    .gt($outer.field(project_units::revision)),
            )
    };
}

// TODO: use vector db instead of postgres
/// Resolve the live production release row (runtime pointer or newest prod by `created_at`). For
/// release workflows and stored snapshot metadata only—not for resolving chat-session unit loads;
/// see the module-level docs on `project_units` vs release metadata.
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

/// Hydrates rows from `project_units` using refs stored on the latest production release row.
///
/// The release row only points at snapshot `(chat_session_id, unit_id)` pairs; it does not replace
/// the database as the source of row content. Prefer explicit session-scoped keys (e.g. finish
/// analysis tools) instead of this when the caller already has the chat session—see module docs.
pub fn load_units_from_latest_production_metadata(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let keys: Vec<(Uuid, String)> = load_latest_production_release_for_project(conn, project_id)?
        .and_then(|r| r.units_metadata)
        .map(|m| {
            m.combined_unit_refs
                .into_iter()
                .map(|r| (r.chat_session_id, r.unit_id))
                .collect()
        })
        .unwrap_or_default();
    load_project_units_by_keys(conn, project_id, &keys)
}

/// Same as [`load_units_from_latest_production_metadata`] for one release id—snapshot refs only.
pub fn load_units_from_release_metadata(
    conn: &mut PooledConn,
    project_id: Uuid,
    release_id: Uuid,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let release: Option<ProjectReleaseResponse> = project_releases::table
        .filter(project_releases::id.eq(release_id))
        .filter(project_releases::project_id.eq(project_id))
        .select(ProjectReleaseResponse::as_select())
        .first(conn)
        .optional()?;
    let keys: Vec<(Uuid, String)> = release
        .and_then(|r| r.units_metadata)
        .map(|m| {
            m.combined_unit_refs
                .into_iter()
                .map(|r| (r.chat_session_id, r.unit_id))
                .collect()
        })
        .unwrap_or_default();
    load_project_units_by_keys(conn, project_id, &keys)
}

/// Resolve the chat session's base production release id (`live_production_release_id` stored at
/// session creation time) and load units from that release, then merge in draft unit tips from the
/// session itself. This gives the "effective unit set" visible to tools inside a chat session:
/// base release units overlaid with any creates/modifies/removes made in the session.
///
/// TODO: Using this function is not a good idea as it loads all units,
/// try to replace it with something which can handle 2000+ units like using dependency graph.
pub fn load_effective_units_for_chat_session(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let base_release_id: Option<Uuid> = chat_sessions::table
        .filter(chat_sessions::id.eq(session_id))
        .select(chat_sessions::live_production_release_id)
        .first::<Option<Uuid>>(conn)
        .optional()?
        .flatten();

    let base_units = if let Some(rid) = base_release_id {
        load_units_from_release_metadata(conn, project_id, rid)?
    } else {
        vec![]
    };

    let draft_tips = load_chat_session_unit_tips_for_merge(conn, project_id, session_id)?;
    Ok(merge_base_with_draft_units(base_units, draft_tips))
}

/// Overlay session draft unit tips on top of base release units. Draft tips replace base units with
/// the same logical key `(context_slug, module_slug, unit_type, unit_name_slug)`, and new drafts
/// (no matching base unit) are appended. Units marked `is_deleted` in the draft are removed.
fn merge_base_with_draft_units(
    base_units: Vec<ProjectUnitResponse>,
    draft_tips: Vec<ProjectUnitResponse>,
) -> Vec<ProjectUnitResponse> {
    if draft_tips.is_empty() {
        return base_units;
    }
    let draft_map: HashMap<LogicalUnitKey, &ProjectUnitResponse> = draft_tips
        .iter()
        .map(|u| (logical_unit_key_from_row(&u.row), u))
        .collect();

    let mut merged: Vec<ProjectUnitResponse> =
        Vec::with_capacity(base_units.len() + draft_tips.len());
    let mut seen_keys: HashSet<LogicalUnitKey> = HashSet::new();

    for base in base_units {
        let key = logical_unit_key_from_row(&base.row);
        if let Some(draft) = draft_map.get(&key) {
            seen_keys.insert(key);
            if !draft.row.is_deleted {
                merged.push((*draft).clone());
            }
        } else {
            merged.push(base);
        }
    }

    for draft in &draft_tips {
        let key = logical_unit_key_from_row(&draft.row);
        if !seen_keys.contains(&key) && !draft.row.is_deleted {
            merged.push(draft.clone());
        }
    }

    merged
}

// TODO: replace with embedding-based semantic search; for now returns all units unfiltered.
pub fn load_units_for_latest_production_release(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    _query: &str,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    load_effective_units_for_chat_session(conn, project_id, chat_session_id)
}

/// Distinct `context_slug` values from the effective unit set for a chat session (base release +
/// session drafts).
pub fn list_distinct_context_slugs_visible_tip_units(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<Vec<String>, diesel::result::Error> {
    let units = load_effective_units_for_chat_session(conn, project_id, session_id)?;
    let mut seen = HashSet::new();
    let mut out: Vec<String> = units
        .into_iter()
        .map(|u| u.context_slug.clone())
        .filter(|c| seen.insert(c.clone()))
        .collect();
    out.sort();
    Ok(out)
}

/// Env var keys for a project grouped by key (all runtimes). Secret values are never loaded.
pub fn list_project_env_key_summaries(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<Vec<RuntimeEnvKeySummary>, diesel::result::Error> {
    let rows: Vec<ProjectRuntimeEnvVarRow> = project_runtime_env_vars::table
        .filter(project_runtime_env_vars::project_id.eq(project_id))
        .select(ProjectRuntimeEnvVarRow::as_select())
        .order((
            project_runtime_env_vars::key.asc(),
            project_runtime_env_vars::runtime_id.asc(),
        ))
        .load(conn)?;

    let mut by_key: HashMap<String, RuntimeEnvKeySummary> = HashMap::new();
    for row in rows {
        let entry = by_key.entry(row.key.clone()).or_insert_with(|| RuntimeEnvKeySummary {
            key: row.key.clone(),
            is_encrypted: false,
        });
        entry.is_encrypted |= row.is_encrypted;
    }
    let mut out: Vec<_> = by_key.into_values().collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// Distinct `context_slug` values from arbitrary unit rows (e.g. a release snapshot), sorted.
pub fn list_distinct_context_slugs_from_units(units: &[ProjectUnitResponse]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out: Vec<String> = units
        .iter()
        .map(|u| u.context_slug.clone())
        .filter(|c| seen.insert(c.clone()))
        .collect();
    out.sort();
    out
}

/// Distinct `(context_slug, module_slug)` pairs from a unit set, limited to `context_slugs`.
fn context_module_pairs_from_units(
    units: &[ProjectUnitResponse],
    context_slugs: &[String],
) -> Vec<(String, String)> {
    if context_slugs.is_empty() {
        return vec![];
    }
    let allow: HashSet<&str> = context_slugs.iter().map(|s| s.as_str()).collect();
    let mut pairs: HashSet<(String, String)> = HashSet::new();
    for u in units {
        if allow.contains(u.context_slug.as_str()) {
            pairs.insert((u.context_slug.clone(), u.module_slug.clone()));
        }
    }
    let mut out: Vec<_> = pairs.into_iter().collect();
    out.sort();
    out
}

/// Builds ordered module lists per context from a set of `(context_slug, module_slug)` pairs.
fn modules_by_context_from_pairs(
    ordered: Vec<String>,
    pairs: Vec<(String, String)>,
) -> Vec<(String, Vec<String>)> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (ctx, module) in pairs {
        map.entry(ctx).or_default().push(module);
    }
    for mods in map.values_mut() {
        mods.sort();
        mods.dedup();
    }
    ordered
        .into_iter()
        .map(|c| {
            let modules = map.remove(&c).unwrap_or_default();
            (c, modules)
        })
        .collect()
}

fn dedup_ordered_context_slugs(context_slugs: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut ordered: Vec<String> = Vec::new();
    for s in context_slugs {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if seen.insert(t.to_string()) {
            ordered.push(t.to_string());
        }
    }
    ordered
}

/// Distinct `(context_slug, module_slug)` pairs from the effective unit set for a chat session,
/// limited to `context_slugs`.
pub fn list_distinct_context_module_pairs_visible_tip_units(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
    context_slugs: &[String],
) -> Result<Vec<(String, String)>, diesel::result::Error> {
    let units = load_effective_units_for_chat_session(conn, project_id, session_id)?;
    Ok(context_module_pairs_from_units(&units, context_slugs))
}

/// Builds ordered module lists per context for a chat session (base release + session drafts).
pub fn modules_by_context_slugs_visible_tip_units(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
    context_slugs: &[String],
) -> Result<Vec<(String, Vec<String>)>, diesel::result::Error> {
    let ordered = dedup_ordered_context_slugs(context_slugs);
    if ordered.is_empty() {
        return Ok(vec![]);
    }
    let pairs = list_distinct_context_module_pairs_visible_tip_units(
        conn, project_id, session_id, &ordered,
    )?;
    Ok(modules_by_context_from_pairs(ordered, pairs))
}

pub fn modules_by_context_slugs_from_units(
    units: &[ProjectUnitResponse],
    context_slugs: &[String],
) -> Vec<(String, Vec<String>)> {
    let ordered = dedup_ordered_context_slugs(context_slugs);
    if ordered.is_empty() {
        return vec![];
    }
    let pairs = context_module_pairs_from_units(units, &ordered);
    modules_by_context_from_pairs(ordered, pairs)
}

/// Project-level variant: builds ordered module lists from the current live production release
/// (not scoped to a chat session). Used by REST APIs that operate outside a session context.
pub fn modules_by_context_slugs_latest_production(
    conn: &mut PooledConn,
    project_id: Uuid,
    context_slugs: &[String],
) -> Result<Vec<(String, Vec<String>)>, diesel::result::Error> {
    let ordered = dedup_ordered_context_slugs(context_slugs);
    if ordered.is_empty() {
        return Ok(vec![]);
    }
    let units = load_units_from_latest_production_metadata(conn, project_id)?;
    Ok(modules_by_context_slugs_from_units(&units, context_slugs))
}

/// Latest tip row per logical unit in `session_id` (no newer revision in that session for the same slugs).
pub fn load_chat_session_unit_tips_for_merge(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let (pu_outer, pu_inner) = diesel::alias!(
        crate::schema::project_units as pu_outer,
        crate::schema::project_units as pu_inner
    );

    pu_outer
        .filter(pu_outer.field(project_units::project_id).eq(project_id))
        .filter(
            pu_outer
                .field(project_units::chat_session_id)
                .eq(session_id),
        )
        .filter(not(exists(
            pu_inner.filter(
                inner_is_newer_same_unit!(pu_outer, pu_inner)
                    .and(pu_inner.field(project_units::project_id).eq(project_id))
                    .and(
                        pu_inner
                            .field(project_units::chat_session_id)
                            .eq(session_id),
                    ),
            ),
        )))
        .select(select_project_unit_row!(pu_outer))
        .load::<ProjectUnitRow>(conn)
        .and_then(|rows| project_units_with_latest_revision(conn, project_id, rows))
}

/// Same logical key as elsewhere in this module (e.g. [`load_unit_tips_by_ids_for_chat_session`]).
fn logical_unit_key_from_row(row: &ProjectUnitRow) -> LogicalUnitKey {
    (
        row.context_slug.clone(),
        row.module_slug.clone(),
        row.unit_type,
        row.unit_name_slug.clone(),
    )
}

/// Draft unit tips for `session_id` as they were **before** the given release-controller message:
/// replays only completed `ToolCreateUnit` / `ToolModifyUnit` / `ToolRemoveUnit` messages ordered
/// before `(cutoff_created_at, cutoff_message_id)`, keeps the last stored unit id per logical path,
/// then loads those draft rows. [`ProjectUnitResponse::latest_revision`] is set to the row revision
/// (the tip at that point in time), matching full-session draft responses where the returned row is
/// always the current tip.
pub fn load_chat_session_draft_units_before_message_cutoff(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
    cutoff_created_at: DateTime<Utc>,
    cutoff_message_id: Uuid,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    let tool_rows: Vec<(ChatMessageType, Option<Value>)> = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .filter(
            chat_messages::type_
                .eq(ChatMessageType::ToolCreateUnit)
                .or(chat_messages::type_.eq(ChatMessageType::ToolModifyUnit))
                .or(chat_messages::type_.eq(ChatMessageType::ToolRemoveUnit)),
        )
        .filter(
            chat_messages::created_at
                .lt(cutoff_created_at)
                .or(chat_messages::created_at
                    .eq(cutoff_created_at)
                    .and(chat_messages::id.lt(cutoff_message_id))),
        )
        .filter(chat_messages::status.eq(ChatMessageStatus::Completed))
        .order_by((chat_messages::created_at.asc(), chat_messages::id.asc()))
        .select((chat_messages::type_, chat_messages::payload))
        .load(conn)?;

    let mut tips: HashMap<LogicalUnitKey, String> = HashMap::new();

    for (type_, payload) in tool_rows {
        let Ok(Some(parsed)) = ChatMessagePayload::from_value(&type_, payload) else {
            continue;
        };
        match parsed {
            ChatMessagePayload::ToolCreateUnit(p) => {
                if p.error.is_none()
                    && let Some(u) = p.created_unit
                {
                    tips.insert(logical_unit_key_from_row(&u.row), u.id.clone());
                }
            }
            ChatMessagePayload::ToolModifyUnit(p) => {
                if p.error.is_none()
                    && let Some(u) = p.revised_unit
                {
                    tips.insert(logical_unit_key_from_row(&u.row), u.id.clone());
                }
            }
            ChatMessagePayload::ToolRemoveUnit(p) => {
                if p.error.is_none()
                    && let Some(u) = p.removed_unit
                {
                    tips.insert(logical_unit_key_from_row(&u.row), u.id.clone());
                }
            }
            _ => {}
        }
    }

    if tips.is_empty() {
        return Ok(vec![]);
    }

    let ids: Vec<String> = tips.into_values().collect();

    let rows: Vec<ProjectUnitRow> = project_units::table
        .filter(project_units::project_id.eq(project_id))
        .filter(project_units::chat_session_id.eq(session_id))
        .filter(project_units::id.eq_any(ids))
        .select(ProjectUnitRow::as_select())
        .load(conn)?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let rev = row.revision;
            ProjectUnitResponse::from_row(row, rev)
        })
        .collect())
}

/// Resolves a `release_controller_box` message in `session_id`, then returns draft unit tips for
/// that session **as of** the point in time immediately before that message (see
/// [`load_chat_session_draft_units_before_message_cutoff`]).
///
/// Outer [`Result::Err`] is a Diesel error. Inner [`Result::Err`] means the message is missing or
/// not a release-controller row.
pub fn load_chat_session_draft_units_as_of_release_controller_message(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
    release_controller_message_id: Uuid,
) -> Result<
    Result<Vec<ProjectUnitResponse>, DraftUnitsAsOfReleaseControllerError>,
    diesel::result::Error,
> {
    let rcb: Option<(ChatMessageType, DateTime<Utc>)> = chat_messages::table
        .filter(chat_messages::id.eq(release_controller_message_id))
        .filter(chat_messages::chat_session_id.eq(session_id))
        .select((chat_messages::type_, chat_messages::created_at))
        .first::<(ChatMessageType, DateTime<Utc>)>(conn)
        .optional()?;

    let Some((typ, created_at)) = rcb else {
        return Ok(Err(DraftUnitsAsOfReleaseControllerError::MessageNotFound));
    };
    if typ != ChatMessageType::ReleaseControllerBox {
        return Ok(Err(
            DraftUnitsAsOfReleaseControllerError::NotReleaseControllerMessage,
        ));
    }

    let units = load_chat_session_draft_units_before_message_cutoff(
        conn,
        project_id,
        session_id,
        created_at,
        release_controller_message_id,
    )?;
    Ok(Ok(units))
}

/// Loads from `project_units` only. Order matches `keys` (omits unknown pairs). Each key is
/// `(chat_session_id, id)` where `id` is the full stored id including revision. Keys must match rows
/// in the table (e.g. refs from release metadata). For ids from the session effective unit set, use
/// [`load_related_units_for_chat_session_by_full_ids`].
pub fn load_project_units_by_keys(
    conn: &mut PooledConn,
    project_id: Uuid,
    keys: &[(Uuid, String)],
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    if keys.is_empty() {
        return Ok(vec![]);
    }
    let mut by_key: HashMap<(Uuid, String), ProjectUnitRow> = HashMap::new();
    for (chat_session_id, id) in keys {
        let row: Option<ProjectUnitRow> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(chat_session_id))
            .filter(project_units::id.eq(id))
            .select(ProjectUnitRow::as_select())
            .first(conn)
            .optional()?;
        if let Some(u) = row {
            by_key.insert((*chat_session_id, id.clone()), u);
        }
    }
    let ordered: Vec<ProjectUnitRow> = keys
        .iter()
        .filter_map(|(s, id)| by_key.remove(&(*s, id.clone())))
        .collect();
    project_units_with_latest_revision(conn, project_id, ordered)
}

pub fn load_related_units_for_chat_session_by_full_ids(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
    full_unit_ids: &[String],
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    if full_unit_ids.is_empty() {
        return Ok(vec![]);
    }
    let effective = load_effective_units_for_chat_session(conn, project_id, session_id)?;
    let by_id: HashMap<String, ProjectUnitResponse> =
        effective.into_iter().map(|u| (u.id.clone(), u)).collect();
    let mut out = Vec::new();
    for id in full_unit_ids {
        if let Some(u) = by_id.get(id) {
            out.push(u.clone());
        }
    }
    Ok(out)
}

/// Latest revision of each logical unit path in the session **effective** set (baseline production
/// release at chat creation plus session creates/modifies/removes). Order follows `paths`; omits
/// paths with no visible unit (including soft-deleted).
pub fn load_effective_units_for_chat_session_by_unit_paths(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_id: Uuid,
    paths: &[UnitPath],
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    if paths.is_empty() {
        return Ok(vec![]);
    }
    let effective = load_effective_units_for_chat_session(conn, project_id, session_id)?;
    let by_key: HashMap<LogicalUnitKey, ProjectUnitResponse> = effective
        .into_iter()
        .map(|u| (logical_unit_key_from_row(&u.row), u))
        .collect();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let key = (
            path.context_slug.clone(),
            path.module_slug.clone(),
            path.unit_type,
            path.unit_name_slug.clone(),
        );
        if let Some(u) = by_key.get(&key) {
            out.push(u.clone());
        }
    }
    Ok(out)
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

#[derive(Debug, Clone)]
pub struct ChatSessionListReleasePair {
    pub env: PlatformEnvironmentType,
    pub revision: f64,
}

#[derive(Debug, Clone)]
pub enum ChatSessionListUnitFilter {
    FullId(String),
    Path(UnitPath),
}

fn pg_escape_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}

fn chat_session_list_extra_where_sql(
    release_pairs: &[ChatSessionListReleasePair],
    unit_filters: &[ChatSessionListUnitFilter],
) -> String {
    let mut out = String::new();
    if !release_pairs.is_empty() {
        out.push_str(
            " AND EXISTS (SELECT 1 FROM project_releases pr WHERE pr.project_id = cs.project_id AND pr.chat_session_id IS NOT DISTINCT FROM cs.id AND (",
        );
        for (i, p) in release_pairs.iter().enumerate() {
            if i > 0 {
                out.push_str(" OR ");
            }
            let is_stage = p.env == PlatformEnvironmentType::Stage;
            let rev = if p.revision.is_finite() {
                p.revision
            } else {
                0.0
            };
            out.push_str(&format!(
                "(pr.is_stage = {} AND pr.revision = {})",
                is_stage, rev
            ));
        }
        out.push_str("))");
    }
    if !unit_filters.is_empty() {
        out.push_str(
            " AND EXISTS (SELECT 1 FROM project_units pu WHERE pu.project_id = cs.project_id AND pu.chat_session_id = cs.id AND (",
        );
        for (i, t) in unit_filters.iter().enumerate() {
            if i > 0 {
                out.push_str(" OR ");
            }
            match t {
                ChatSessionListUnitFilter::FullId(id) => {
                    out.push_str(&format!("pu.id = '{}'", pg_escape_literal(id)));
                }
                ChatSessionListUnitFilter::Path(p) => {
                    out.push_str(&format!(
                        "(pu.context_slug = '{}' AND pu.module_slug = '{}' AND pu.unit_type = '{}'::project_unit_type AND pu.unit_name_slug = '{}')",
                        pg_escape_literal(&p.context_slug),
                        pg_escape_literal(&p.module_slug),
                        p.unit_type.as_slug(),
                        pg_escape_literal(&p.unit_name_slug),
                    ));
                }
            }
        }
        out.push_str("))");
    }
    out
}

#[derive(QueryableByName)]
struct ChatSessionListCountRow {
    #[diesel(sql_type = sql_types::BigInt)]
    count: i64,
}

#[derive(QueryableByName)]
struct ChatSessionListIdRow {
    #[diesel(sql_type = sql_types::Uuid)]
    id: Uuid,
}

/// Same `project_releases` linkage as [`load_owned_releases_by_chat_session`] and session-scoped
/// `project_units` rows as in list payloads (path filter = any revision for that logical unit).
pub fn list_project_chat_sessions_page(
    conn: &mut PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    release_pairs: &[ChatSessionListReleasePair],
    unit_filters: &[ChatSessionListUnitFilter],
    page: u32,
    page_size: u32,
) -> Result<(Vec<Uuid>, i64, u32, u32), diesel::result::Error> {
    let page = page.max(1);
    let page_size = page_size.clamp(1, 100);
    let offset = (page.saturating_sub(1) as i64).saturating_mul(page_size as i64);
    let extra = chat_session_list_extra_where_sql(release_pairs, unit_filters);

    let count_sql = format!(
        "SELECT COUNT(*)::bigint AS count FROM chat_sessions cs \
         INNER JOIN projects p ON p.id = cs.project_id \
         WHERE p.owner_user_id = $1 AND cs.project_id = $2{extra}"
    );
    let total = diesel::sql_query(&count_sql)
        .bind::<sql_types::Uuid, _>(user_id)
        .bind::<sql_types::Uuid, _>(project_id)
        .get_result::<ChatSessionListCountRow>(conn)?
        .count;

    let list_sql = format!(
        "SELECT cs.id AS id FROM chat_sessions cs \
         INNER JOIN projects p ON p.id = cs.project_id \
         WHERE p.owner_user_id = $1 AND cs.project_id = $2{extra} \
         ORDER BY cs.created_at DESC, cs.id DESC \
         LIMIT {page_size} OFFSET {offset}"
    );
    let ids: Vec<Uuid> = diesel::sql_query(&list_sql)
        .bind::<sql_types::Uuid, _>(user_id)
        .bind::<sql_types::Uuid, _>(project_id)
        .load::<ChatSessionListIdRow>(conn)?
        .into_iter()
        .map(|r| r.id)
        .collect();

    Ok((ids, total, page, page_size))
}

/// Releases for `project_id` whose `chat_session_id` is in `session_ids`, grouped by session (newest first per group).
pub fn load_owned_releases_by_chat_session(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<ProjectReleaseResponse>>, diesel::result::Error> {
    if session_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<ProjectReleaseResponse> = project_releases::table
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::chat_session_id.eq_any(session_ids))
        .order(project_releases::created_at.desc())
        .select(ProjectReleaseResponse::as_select())
        .load(conn)?;

    let mut map: HashMap<Uuid, Vec<ProjectReleaseResponse>> =
        session_ids.iter().map(|&id| (id, Vec::new())).collect();

    for r in rows {
        if let Some(sid) = r.chat_session_id
            && let Some(vec) = map.get_mut(&sid)
        {
            vec.push(r);
        }
    }
    Ok(map)
}

/// Latest revision per logical unit for each session (same “no newer revision exists” pattern as
/// [`load_chat_session_unit_tips_for_merge`]).
pub fn load_owned_unit_tips_for_chat_sessions(
    conn: &mut PooledConn,
    project_id: Uuid,
    session_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<ProjectUnitResponse>>, diesel::result::Error> {
    if session_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let (pu_outer, pu_inner) = diesel::alias!(
        crate::schema::project_units as pu_outer,
        crate::schema::project_units as pu_inner
    );

    let rows: Vec<ProjectUnitRow> = pu_outer
        .filter(pu_outer.field(project_units::project_id).eq(project_id))
        .filter(
            pu_outer
                .field(project_units::chat_session_id)
                .eq_any(session_ids),
        )
        .filter(not(exists(
            pu_inner.filter(
                inner_is_newer_same_unit!(pu_outer, pu_inner)
                    .and(pu_inner.field(project_units::project_id).eq(project_id)),
            ),
        )))
        .select(select_project_unit_row!(pu_outer))
        .load::<ProjectUnitRow>(conn)?;

    let with_latest = project_units_with_latest_revision(conn, project_id, rows)?;

    let mut by_session: HashMap<Uuid, Vec<ProjectUnitResponse>> = HashMap::new();
    for u in with_latest {
        by_session.entry(u.chat_session_id).or_default().push(u);
    }
    for vec in by_session.values_mut() {
        vec.sort_by(|a, b| {
            a.context_slug
                .cmp(&b.context_slug)
                .then_with(|| a.module_slug.cmp(&b.module_slug))
                .then_with(|| a.unit_name_slug.cmp(&b.unit_name_slug))
        });
    }
    Ok(by_session)
}

/// For each stored `unit_id` (full id including revision), resolves the **revision tip** for that
/// logical unit in `chat_session_id` (same “no newer revision” rule as
/// [`load_owned_unit_tips_for_chat_sessions`]) and returns [`ProjectUnitResponse`] with
/// `latest_revision` set. Order matches `unit_ids`; `None` when the id is unknown in that session.
pub fn load_unit_tips_by_ids_for_chat_session(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    unit_ids: &[String],
) -> Result<Vec<Option<ProjectUnitResponse>>, diesel::result::Error> {
    if unit_ids.is_empty() {
        return Ok(vec![]);
    }

    let mut tips_by_key: HashMap<LogicalUnitKey, ProjectUnitResponse> = HashMap::new();
    let tips_map = load_owned_unit_tips_for_chat_sessions(conn, project_id, &[chat_session_id])?;
    if let Some(vec) = tips_map.get(&chat_session_id) {
        for u in vec {
            tips_by_key.insert(logical_unit_key_from_row(&u.row), u.clone());
        }
    }

    let mut out = Vec::with_capacity(unit_ids.len());
    for id in unit_ids {
        let anchor: Option<ProjectUnitRow> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(chat_session_id))
            .filter(project_units::id.eq(id))
            .select(ProjectUnitRow::as_select())
            .first(conn)
            .optional()?;
        let Some(a) = anchor else {
            out.push(None);
            continue;
        };
        out.push(tips_by_key.get(&logical_unit_key_from_row(&a)).cloned());
    }
    Ok(out)
}

/// For each stored `unit_id` (full id including revision), returns the **exact** row at that id in
/// `chat_session_id`, with [`ProjectUnitResponse::latest_revision`] set to the maximum revision for
/// that logical unit under `project_id` (same as [`project_units_with_latest_revision`]). Unlike
/// [`load_unit_tips_by_ids_for_chat_session`], does not substitute the revision **tip** row.
pub fn load_units_by_ids_for_chat_session(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    unit_ids: &[String],
) -> Result<Vec<Option<ProjectUnitResponse>>, diesel::result::Error> {
    if unit_ids.is_empty() {
        return Ok(vec![]);
    }

    let mut indexed_rows: Vec<(usize, ProjectUnitRow)> = Vec::new();
    for (i, id) in unit_ids.iter().enumerate() {
        let anchor: Option<ProjectUnitRow> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(chat_session_id))
            .filter(project_units::id.eq(id))
            .select(ProjectUnitRow::as_select())
            .first(conn)
            .optional()?;
        if let Some(a) = anchor {
            indexed_rows.push((i, a));
        }
    }

    if indexed_rows.is_empty() {
        return Ok(unit_ids.iter().map(|_| None).collect());
    }

    let rows: Vec<ProjectUnitRow> = indexed_rows.iter().map(|(_, r)| r.clone()).collect();
    let with_latest = project_units_with_latest_revision(conn, project_id, rows)?;

    let mut out: Vec<Option<ProjectUnitResponse>> = vec![None; unit_ids.len()];
    for ((i, _), resp) in indexed_rows.into_iter().zip(with_latest) {
        out[i] = Some(resp);
    }
    Ok(out)
}

/// For each [`UnitPath`], returns `MAX(revision)` in this chat session (same project), or `None`
/// when no row exists.
pub fn load_latest_revisions_by_unit_paths(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    paths: &[UnitPath],
) -> Result<Vec<Option<i32>>, diesel::result::Error> {
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let max_rev: Option<i32> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(chat_session_id))
            .filter(project_units::context_slug.eq(&path.context_slug))
            .filter(project_units::module_slug.eq(&path.module_slug))
            .filter(project_units::unit_type.eq(path.unit_type))
            .filter(project_units::unit_name_slug.eq(&path.unit_name_slug))
            .select(max(project_units::revision))
            .first(conn)?;
        out.push(max_rev);
    }
    Ok(out)
}

/// For each [`UnitPath`], returns the unit row from this chat session’s baseline production release
/// (`chat_sessions.live_production_release_id`): the snapshot row in that release’s
/// `combinedUnitIDs` for the same logical unit (same slugs and type), with `latest_revision` filled
/// like [`load_project_units_by_keys`]. `None` when the session has no baseline, the baseline row is
/// missing or not a production release, the release has no combined snapshot, or the path is not in
/// that snapshot.
pub fn load_pre_chat_base_units_by_unit_paths(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    paths: &[UnitPath],
) -> Result<Vec<Option<ProjectUnitResponse>>, diesel::result::Error> {
    if paths.is_empty() {
        return Ok(vec![]);
    }

    let baseline_id: Option<Uuid> = chat_sessions::table
        .filter(chat_sessions::id.eq(chat_session_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .select(chat_sessions::live_production_release_id)
        .first::<Option<Uuid>>(conn)?;

    let Some(release_id) = baseline_id else {
        return Ok(vec![None; paths.len()]);
    };

    let release_ok: Option<Uuid> = project_releases::table
        .filter(project_releases::id.eq(release_id))
        .filter(project_releases::project_id.eq(project_id))
        .filter(project_releases::is_stage.eq(false))
        .select(project_releases::id)
        .first(conn)
        .optional()?;

    let Some(release_id) = release_ok else {
        return Ok(vec![None; paths.len()]);
    };

    let baseline_units = load_units_from_release_metadata(conn, project_id, release_id)?;
    let mut by_path: HashMap<(String, String, ProjectUnitType, String), ProjectUnitResponse> =
        HashMap::new();
    for u in baseline_units {
        let key = (
            u.context_slug.clone(),
            u.module_slug.clone(),
            u.unit_type,
            u.unit_name_slug.clone(),
        );
        let replace = match by_path.get(&key) {
            None => true,
            Some(existing) => u.revision > existing.revision,
        };
        if replace {
            by_path.insert(key, u);
        }
    }

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let key = (
            path.context_slug.clone(),
            path.module_slug.clone(),
            path.unit_type,
            path.unit_name_slug.clone(),
        );
        out.push(by_path.get(&key).cloned());
    }
    Ok(out)
}

fn baseline_rows_with_id<'a>(
    baseline_units: &'a [ProjectUnitResponse],
    unit_id: &str,
) -> Vec<&'a ProjectUnitResponse> {
    baseline_units.iter().filter(|u| u.id == unit_id).collect()
}

fn pick_baseline_unit_row_for_id(
    baseline_units: &[ProjectUnitResponse],
    unit_id: &str,
    prefer_chat_session_id: Uuid,
) -> Option<ProjectUnitRow> {
    let cand = baseline_rows_with_id(baseline_units, unit_id);
    match cand.len() {
        0 => None,
        1 => Some(cand[0].row.clone()),
        _ => {
            let same_session: Vec<_> = cand
                .iter()
                .copied()
                .filter(|u| u.chat_session_id == prefer_chat_session_id)
                .collect();
            if same_session.len() == 1 {
                Some(same_session[0].row.clone())
            } else {
                None
            }
        }
    }
}

pub fn load_unit_previous_revisions_by_full_ids_for_chat_session(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    full_unit_ids: &[String],
) -> Result<(Vec<Option<ProjectUnitResponse>>, Option<String>), diesel::result::Error> {
    if full_unit_ids.is_empty() {
        return Ok((vec![], None));
    }

    let baseline_id: Option<Uuid> = chat_sessions::table
        .filter(chat_sessions::id.eq(chat_session_id))
        .filter(chat_sessions::project_id.eq(project_id))
        .select(chat_sessions::live_production_release_id)
        .first::<Option<Uuid>>(conn)?;

    let baseline_units: Vec<ProjectUnitResponse> = if let Some(rid) = baseline_id {
        let release_ok: Option<Uuid> = project_releases::table
            .filter(project_releases::id.eq(rid))
            .filter(project_releases::project_id.eq(project_id))
            .filter(project_releases::is_stage.eq(false))
            .select(project_releases::id)
            .first(conn)
            .optional()?;
        if let Some(rid) = release_ok {
            load_units_from_release_metadata(conn, project_id, rid)?
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    let n = full_unit_ids.len();
    let mut out = vec![None; n];
    let mut rows_for_latest: Vec<ProjectUnitRow> = Vec::new();
    let mut pending: Vec<usize> = Vec::new();

    for (i, full_id) in full_unit_ids.iter().enumerate() {
        let path: UnitPath = match parse_unit_path(full_id) {
            Ok(p) => p,
            Err(_) => {
                return Ok((vec![], Some(full_id.clone())));
            }
        };
        let parts: Vec<&str> = full_id.split('.').filter(|p| !p.is_empty()).collect();
        let rev: i32 = match parts.get(4).and_then(|s| s.parse().ok()) {
            Some(r) if r >= 1 => r,
            _ => {
                return Ok((vec![], Some(full_id.clone())));
            }
        };

        if rev <= 1 {
            continue;
        }

        let prev_id = compose_unit_id(
            &path.context_slug,
            &path.module_slug,
            path.unit_type.as_slug(),
            &path.unit_name_slug,
            rev - 1,
        );

        let prev_row: Option<ProjectUnitRow> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(chat_session_id))
            .filter(project_units::id.eq(&prev_id))
            .select(ProjectUnitRow::as_select())
            .first(conn)
            .optional()?;

        if let Some(row) = prev_row {
            pending.push(i);
            rows_for_latest.push(row);
            continue;
        }

        if let Some(row) = pick_baseline_unit_row_for_id(&baseline_units, &prev_id, chat_session_id)
        {
            pending.push(i);
            rows_for_latest.push(row);
            continue;
        }

        return Ok((vec![], Some(full_id.clone())));
    }

    if rows_for_latest.is_empty() {
        return Ok((out, None));
    }

    let with_latest = project_units_with_latest_revision(conn, project_id, rows_for_latest)?;
    for (out_idx, resp) in pending.into_iter().zip(with_latest) {
        out[out_idx] = Some(resp);
    }

    Ok((out, None))
}
