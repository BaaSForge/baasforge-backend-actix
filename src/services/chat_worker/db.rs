use crate::database::PooledConn;
use crate::libs::unit_slug::UnitPath;
use crate::models::projects::{
    ProjectReleaseResponse, ProjectUnitResponse, ProjectUnitRow, ProjectUnitType,
};
use crate::schema::{project_releases, project_units};
use diesel::dsl::{exists, max, min, not};
use diesel::prelude::*;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Columns for [`ProjectUnitRow`] when selecting from a `project_units` alias (order matches the struct).
macro_rules! select_project_unit_row {
    ($a:expr) => {
        (
            $a.field(project_units::project_id),
            $a.field(project_units::chat_session_id),
            $a.field(project_units::id),
            $a.field(project_units::revision),
            $a.field(project_units::code),
            $a.field(project_units::explanation),
            $a.field(project_units::is_draft),
            $a.field(project_units::unit_type),
            $a.field(project_units::is_deleted),
            $a.field(project_units::context_slug),
            $a.field(project_units::module_slug),
            $a.field(project_units::unit_name_slug),
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

    let aggregates: Vec<(
        Uuid,
        String,
        String,
        ProjectUnitType,
        String,
        Option<i32>,
    )> = project_units::table
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

/// `NOT is_draft` **or** row belongs to this chat session (draft visibility for cross-session reads).
macro_rules! release_visibility {
    ($a:expr, $chat_session_id:expr) => {
        $a.field(project_units::is_draft)
            .eq(false)
            .or($a.field(project_units::chat_session_id).eq($chat_session_id))
    };
}

// TODO: use vector db instead of postgres
/// Load units that belong to the latest production release for the project.
/// Finds the production runtime's live_release_version, then the matching release, then units from its units_metadata.
///
/// Draft revisions are only included when they belong to `chat_session_id`, so other sessions' drafts are never exposed.
///
/// Only the tip of each revision chain is returned: among rows visible under the draft rule, we keep
/// the largest `revision` per logical unit `(chat_session_id, context_slug, module_slug, unit_type, unit_name_slug)`.
/// Stored `id` includes revision as the last dot-separated segment, so we group by slug columns, not by `id`.
pub fn load_units_for_latest_production_release(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    query: &str,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
    tracing::debug!(query = %query, "load_units_for_latest_production_release");

    let (pu_outer, pu_inner) = diesel::alias!(
        crate::schema::project_units as pu_outer,
        crate::schema::project_units as pu_inner
    );

    pu_outer
        .filter(pu_outer.field(project_units::project_id).eq(project_id))
        .filter(release_visibility!(pu_outer, chat_session_id))
        .filter(not(exists(
            pu_inner.filter(
                inner_is_newer_same_unit!(pu_outer, pu_inner)
                    .and(pu_inner.field(project_units::project_id).eq(project_id))
                    .and(release_visibility!(pu_inner, chat_session_id)),
            ),
        )))
        .select(select_project_unit_row!(pu_outer))
        .load::<ProjectUnitRow>(conn)
        .and_then(|rows| project_units_with_latest_revision(conn, project_id, rows))
}

/// Distinct `context_slug` values among visible tip units (same visibility and revision-tip rules as
/// [`load_units_for_latest_production_release`]).
pub fn list_distinct_context_slugs_visible_tip_units(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
) -> Result<Vec<String>, diesel::result::Error> {
    let (pu_outer, pu_inner) = diesel::alias!(
        crate::schema::project_units as pu_outer,
        crate::schema::project_units as pu_inner
    );

    pu_outer
        .filter(pu_outer.field(project_units::project_id).eq(project_id))
        .filter(release_visibility!(pu_outer, chat_session_id))
        .filter(not(exists(
            pu_inner.filter(
                inner_is_newer_same_unit!(pu_outer, pu_inner)
                    .and(pu_inner.field(project_units::project_id).eq(project_id))
                    .and(release_visibility!(pu_inner, chat_session_id)),
            ),
        )))
        .select(pu_outer.field(project_units::context_slug))
        .distinct()
        .order_by(pu_outer.field(project_units::context_slug))
        .load::<String>(conn)
}

/// Distinct `(context_slug, module_slug)` pairs for the given contexts, using the same visible tip rules
/// as [`load_units_for_latest_production_release`].
pub fn list_distinct_context_module_pairs_visible_tip_units(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    context_slugs: &[String],
) -> Result<Vec<(String, String)>, diesel::result::Error> {
    if context_slugs.is_empty() {
        return Ok(vec![]);
    }
    let (pu_outer, pu_inner) = diesel::alias!(
        crate::schema::project_units as pu_outer,
        crate::schema::project_units as pu_inner
    );

    pu_outer
        .filter(pu_outer.field(project_units::project_id).eq(project_id))
        .filter(release_visibility!(pu_outer, chat_session_id))
        .filter(
            pu_outer
                .field(project_units::context_slug)
                .eq_any(context_slugs),
        )
        .filter(not(exists(
            pu_inner.filter(
                inner_is_newer_same_unit!(pu_outer, pu_inner)
                    .and(pu_inner.field(project_units::project_id).eq(project_id))
                    .and(release_visibility!(pu_inner, chat_session_id)),
            ),
        )))
        .select((
            pu_outer.field(project_units::context_slug),
            pu_outer.field(project_units::module_slug),
        ))
        .distinct()
        .order_by((
            pu_outer.field(project_units::context_slug),
            pu_outer.field(project_units::module_slug),
        ))
        .load::<(String, String)>(conn)
}

/// Builds ordered module lists per context (one entry per `contexts` element, stable order, deduped request).
pub fn modules_by_context_slugs_visible_tip_units(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    context_slugs: &[String],
) -> Result<Vec<(String, Vec<String>)>, diesel::result::Error> {
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
    if ordered.is_empty() {
        return Ok(vec![]);
    }
    let pairs = list_distinct_context_module_pairs_visible_tip_units(
        conn,
        project_id,
        chat_session_id,
        &ordered,
    )?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (ctx, module) in pairs {
        map.entry(ctx).or_default().push(module);
    }
    for mods in map.values_mut() {
        mods.sort();
        mods.dedup();
    }
    Ok(ordered
        .into_iter()
        .map(|c| {
            let modules = map.remove(&c).unwrap_or_default();
            (c, modules)
        })
        .collect())
}

/// Draft units for a chat session: one row per logical unit (max `revision` among draft rows in that session).
pub fn load_chat_session_draft_units_latest_revision(
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
        .filter(pu_outer.field(project_units::chat_session_id).eq(session_id))
        .filter(pu_outer.field(project_units::is_draft).eq(true))
        .filter(not(exists(
            pu_inner.filter(
                inner_is_newer_same_unit!(pu_outer, pu_inner)
                    .and(pu_inner.field(project_units::project_id).eq(project_id))
                    .and(pu_inner.field(project_units::chat_session_id).eq(session_id))
                    .and(pu_inner.field(project_units::is_draft).eq(true)),
            ),
        )))
        .select(select_project_unit_row!(pu_outer))
        .load::<ProjectUnitRow>(conn)
        .and_then(|rows| project_units_with_latest_revision(conn, project_id, rows))
}

/// Order matches `keys` (omits unknown pairs). Each key is `(chat_session_id, id)`; `id` is the full stored id including revision.
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

    let mut map: HashMap<Uuid, Vec<ProjectReleaseResponse>> = session_ids
        .iter()
        .map(|&id| (id, Vec::new()))
        .collect();

    for r in rows {
        if let Some(sid) = r.chat_session_id {
            if let Some(vec) = map.get_mut(&sid) {
                vec.push(r);
            }
        }
    }
    Ok(map)
}

/// Latest revision per logical unit for each session (same “no newer revision exists” pattern as
/// [`load_chat_session_draft_units_latest_revision`], without filtering on `is_draft`).
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
        .filter(pu_outer.field(project_units::chat_session_id).eq_any(session_ids))
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

type LogicalUnitKey = (String, String, ProjectUnitType, String);

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
            let key = (
                u.context_slug.clone(),
                u.module_slug.clone(),
                u.unit_type,
                u.unit_name_slug.clone(),
            );
            tips_by_key.insert(key, u.clone());
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
        let key = (
            a.context_slug,
            a.module_slug,
            a.unit_type,
            a.unit_name_slug,
        );
        out.push(tips_by_key.get(&key).cloned());
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

/// For each [`UnitPath`], returns the unit row at revision `MIN(revision) - 1` in this chat session
/// (the revision that was current before the first new revision in the session). If there is no row
/// for the path, or the minimum revision is `1`, returns `None`. Rows are wrapped with
/// [`project_units_with_latest_revision`] like other session unit loaders.
pub fn load_pre_chat_base_units_by_unit_paths(
    conn: &mut PooledConn,
    project_id: Uuid,
    chat_session_id: Uuid,
    paths: &[UnitPath],
) -> Result<Vec<Option<ProjectUnitResponse>>, diesel::result::Error> {
    if paths.is_empty() {
        return Ok(vec![]);
    }

    let mut indexed: Vec<(usize, ProjectUnitRow)> = Vec::new();
    for (i, path) in paths.iter().enumerate() {
        let min_rev: Option<i32> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(chat_session_id))
            .filter(project_units::context_slug.eq(&path.context_slug))
            .filter(project_units::module_slug.eq(&path.module_slug))
            .filter(project_units::unit_type.eq(path.unit_type))
            .filter(project_units::unit_name_slug.eq(&path.unit_name_slug))
            .select(min(project_units::revision))
            .first(conn)?;

        let Some(m) = min_rev else {
            continue;
        };
        if m <= 1 {
            continue;
        }
        let prev = m - 1;
        let base: Option<ProjectUnitRow> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(chat_session_id))
            .filter(project_units::context_slug.eq(&path.context_slug))
            .filter(project_units::module_slug.eq(&path.module_slug))
            .filter(project_units::unit_type.eq(path.unit_type))
            .filter(project_units::unit_name_slug.eq(&path.unit_name_slug))
            .filter(project_units::revision.eq(prev))
            .select(ProjectUnitRow::as_select())
            .first(conn)
            .optional()?;
        if let Some(row) = base {
            indexed.push((i, row));
        }
    }

    if indexed.is_empty() {
        return Ok(vec![None; paths.len()]);
    }

    let rows_only: Vec<ProjectUnitRow> = indexed.iter().map(|(_, r)| r.clone()).collect();
    let with_latest = project_units_with_latest_revision(conn, project_id, rows_only)?;

    let mut out: Vec<Option<ProjectUnitResponse>> = vec![None; paths.len()];
    for ((i, _), resp) in indexed.into_iter().zip(with_latest) {
        out[i] = Some(resp);
    }
    Ok(out)
}
