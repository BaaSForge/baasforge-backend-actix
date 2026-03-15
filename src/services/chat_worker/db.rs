use crate::database::PooledConn;
use crate::models::chat_messages::{ChatMessageRow, ChatMessageStatus, ChatSessionRoleType};
use crate::models::projects::{ProjectUnitResponse};
use crate::schema::{chat_messages, project_units};
use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

pub fn load_previous_session_messages(
    conn: &mut PooledConn,
    session_id: Uuid,
    limit: i64,
) -> Result<Vec<(ChatSessionRoleType, Option<Value>)>, diesel::result::Error> {
    let rows = chat_messages::table
        .filter(chat_messages::chat_session_id.eq(session_id))
        .order(chat_messages::id.asc())
        .filter(chat_messages::status.eq(ChatMessageStatus::Completed))
        .limit(limit)
        .select(ChatMessageRow::as_select())
        .load::<ChatMessageRow>(conn)?;

    let out = rows
        .into_iter()
        .map(|row| (row.role, row.payload))
        .collect();
    Ok(out)
}

// TODO: use vector db instead of postgres
/// Load units that belong to the latest production release for the project.
/// Finds the production runtime's live_release_version, then the matching release, then units from its units_metadata.
pub fn load_units_for_latest_production_release(
    conn: &mut PooledConn,
    project_id: Uuid,
) -> Result<Vec<ProjectUnitResponse>, diesel::result::Error> {
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
        // .filter(project_units::id.eq_any(&unit_ids))
        .select(ProjectUnitResponse::as_select())
        .load::<ProjectUnitResponse>(conn)
}
