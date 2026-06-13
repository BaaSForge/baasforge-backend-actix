use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Nullable, Timestamptz, Uuid as SqlUuid};
use diesel::QueryableByName;
use uuid::Uuid;

use crate::models::projects::PlatformEnvironmentType;
use crate::schema::{project_runtimes, projects, users};

#[derive(Debug, Clone)]
pub struct DueProjectRow {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub last_cost_calculated_at: Option<DateTime<Utc>>,
}

#[derive(QueryableByName)]
struct DueProjectQueryRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
    #[diesel(sql_type = SqlUuid)]
    owner_user_id: Uuid,
    #[diesel(sql_type = Nullable<Timestamptz>)]
    last_cost_calculated_at: Option<DateTime<Utc>>,
}

const LIST_DUE_PROJECTS_SQL: &str = "
SELECT id, owner_user_id, last_cost_calculated_at
FROM projects
WHERE last_cost_calculated_at IS NULL OR last_cost_calculated_at < $1
ORDER BY last_cost_calculated_at NULLS FIRST, id
LIMIT $2
";

pub fn list_due_projects<C>(
    conn: &mut C,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<DueProjectRow>, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    sql_query(LIST_DUE_PROJECTS_SQL)
        .bind::<Timestamptz, _>(now)
        .bind::<BigInt, _>(limit)
        .load::<DueProjectQueryRow>(conn)
        .map(|rows| {
            rows.into_iter()
                .map(|row| DueProjectRow {
                    id: row.id,
                    owner_user_id: row.owner_user_id,
                    last_cost_calculated_at: row.last_cost_calculated_at,
                })
                .collect()
        })
}

pub fn advance_platform_cost_window<C>(
    conn: &mut C,
    project_id: Uuid,
    expected_previous: Option<DateTime<Utc>>,
    next_at: DateTime<Utc>,
) -> Result<bool, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let updated = match expected_previous {
        None => diesel::update(
            projects::table
                .filter(projects::id.eq(project_id))
                .filter(projects::last_cost_calculated_at.is_null()),
        )
        .set(projects::last_cost_calculated_at.eq(next_at))
        .execute(conn)?,
        Some(previous) => diesel::update(
            projects::table
                .filter(projects::id.eq(project_id))
                .filter(projects::last_cost_calculated_at.eq(previous)),
        )
        .set(projects::last_cost_calculated_at.eq(next_at))
        .execute(conn)?,
    };
    Ok(updated > 0)
}

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

const LIVE_UNIT_COUNT_SQL: &str = "
SELECT COUNT(*)::bigint AS count
FROM (
    SELECT DISTINCT ON (chat_session_id, id) id
    FROM project_units
    WHERE project_id = $1
      AND is_deleted = false
    ORDER BY chat_session_id, id, revision DESC
) live_units
";

pub fn count_live_units<C>(conn: &mut C, project_id: Uuid) -> Result<i64, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    sql_query(LIVE_UNIT_COUNT_SQL)
        .bind::<SqlUuid, _>(project_id)
        .get_result::<CountRow>(conn)
        .map(|row| row.count)
}

pub fn count_stage_runtimes<C>(conn: &mut C, project_id: Uuid) -> Result<i64, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    project_runtimes::table
        .filter(project_runtimes::project_id.eq(project_id))
        .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Stage))
        .count()
        .get_result::<i64>(conn)
}

pub fn user_has_paid_subscription<C>(
    conn: &mut C,
    user_id: Uuid,
) -> Result<bool, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let (_, largest_tier) = crate::services::payment::get_user_active_paid_packages(conn, user_id)?;
    Ok(largest_tier > 1)
}

pub fn user_package_tier<C>(conn: &mut C, user_id: Uuid) -> Result<i32, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let (_, largest_tier) = crate::services::payment::get_user_active_paid_packages(conn, user_id)?;
    Ok(if largest_tier <= 0 { 1 } else { largest_tier })
}

pub fn try_claim_subscription_cost_window<C>(
    conn: &mut C,
    user_id: Uuid,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> Result<bool, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let updated = diesel::update(
        users::table
            .filter(users::id.eq(user_id))
            .filter(
                users::last_subscription_cost_calculated_at
                    .is_null()
                    .or(users::last_subscription_cost_calculated_at.lt(period_start)),
            ),
    )
    .set(users::last_subscription_cost_calculated_at.eq(period_end))
    .execute(conn)?;
    Ok(updated > 0)
}
