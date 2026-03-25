use crate::database::PooledConn;
use crate::models::projects::{ProjectReleaseResponse, ProjectUnitResponse};
use crate::schema::{project_releases, project_units};
use diesel::dsl::{exists, not};
use diesel::prelude::*;
use std::collections::HashMap;
use uuid::Uuid;

/// Columns for [`ProjectUnitResponse`] when selecting from a `project_units` alias (order matches the struct).
macro_rules! select_project_unit_response {
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
        .select(select_project_unit_response!(pu_outer))
        .load::<ProjectUnitResponse>(conn)
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
        .select(select_project_unit_response!(pu_outer))
        .load::<ProjectUnitResponse>(conn)
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
    let mut by_key: HashMap<(Uuid, String), ProjectUnitResponse> = HashMap::new();
    for (chat_session_id, id) in keys {
        let row: Option<ProjectUnitResponse> = project_units::table
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::chat_session_id.eq(chat_session_id))
            .filter(project_units::id.eq(id))
            .select(ProjectUnitResponse::as_select())
            .first(conn)
            .optional()?;
        if let Some(u) = row {
            by_key.insert((*chat_session_id, id.clone()), u);
        }
    }
    Ok(keys
        .iter()
        .filter_map(|(s, id)| by_key.remove(&(*s, id.clone())))
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
