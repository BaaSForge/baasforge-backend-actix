use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use diesel::deserialize::{self, FromSql};
use diesel::pg::Pg;
use diesel::serialize::{self, IsNull, Output, ToSql};
use diesel::sql_types::Jsonb;
use serde::Deserialize;
use serde::Serialize;
use std::io::Write;
use std::ops::Deref;
use uuid::Uuid;

use crate::services::units::UnitMetadata;

impl FromSql<Jsonb, Pg> for UnitMetadata {
    fn from_sql(value: diesel::pg::PgValue<'_>) -> deserialize::Result<Self> {
        let v = <serde_json::Value as FromSql<Jsonb, Pg>>::from_sql(value)?;
        serde_json::from_value(v).map_err(|e| Box::new(e) as _)
    }
}

impl ToSql<Jsonb, Pg> for UnitMetadata {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        // JSONB wire format: version byte 1 followed by the JSON bytes.
        out.write_all(&[1])?;
        serde_json::to_writer(out, self)
            .map(|_| IsNull::No)
            .map_err(Into::into)
    }
}

#[derive(Debug, PartialEq, Eq, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::PlatformEnvironmentType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum PlatformEnvironmentType {
    Stage,
    Prod,
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, Hash, diesel_derive_enum::DbEnum, Serialize, Deserialize,
)]
#[ExistingTypePath = "crate::schema::sql_types::ProjectUnitType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ProjectUnitType {
    Api,
    Workflow,
    QueueWorker,
    Actor,
}

impl ProjectUnitType {
    pub fn as_slug(&self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Workflow => "workflow",
            Self::QueueWorker => "queue_worker",
            Self::Actor => "actor",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Api, Self::Workflow, Self::QueueWorker, Self::Actor]
    }

    /// Parses the slug used in composite unit ids (`context.module.<slug>.…`). Inverse of [`Self::as_slug`].
    pub fn from_slug(s: &str) -> Option<Self> {
        Self::all().iter().copied().find(|t| t.as_slug() == s)
    }
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::project_units)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectUnitInsert {
    pub project_id: Uuid,
    pub chat_session_id: Uuid,
    pub id: String,
    pub revision: i32,
    pub code: Option<String>,
    pub compiled_code: Option<String>,
    pub explanation: Option<String>,
    pub unit_type: ProjectUnitType,
    pub is_deleted: bool,
    pub context_slug: String,
    pub module_slug: String,
    pub unit_name_slug: String,
    pub metadata: Option<UnitMetadata>,
    pub depends_on: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectInsert {
    pub owner_user_id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::project_runtimes)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeInsert {
    pub project_id: Uuid,
    pub environment: PlatformEnvironmentType,
    pub name: Option<String>,
    pub slug: Option<String>,
}

#[derive(Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRuntimesBaseUrls {
    pub api: String,
    pub workflow: String,
    pub actor: String,
    pub queue_worker: String,
}

impl ProjectRuntimesBaseUrls {
    pub fn uniform(base: &str) -> Self {
        let b = base.to_string();
        Self {
            api: b.clone(),
            workflow: b.clone(),
            actor: b.clone(),
            queue_worker: b,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct ProjectDetailResponse {
    #[serde(flatten)]
    pub project: ProjectResponse,
    pub runtimes: ProjectRuntimesBaseUrls,
}

/// Row mapped 1:1 to `project_units` (Diesel query/returning). Shared as the inner payload of
/// [`ProjectUnitResponse`] so API/tool JSON stays a flat object via `#[serde(flatten)]`.
#[derive(Debug, Clone, PartialEq, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = crate::schema::project_units)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectUnitRow {
    pub project_id: Uuid,
    pub chat_session_id: Uuid,
    pub id: String,
    pub revision: i32,
    pub code: Option<String>,
    pub compiled_code: Option<String>,
    pub explanation: Option<String>,
    pub unit_type: ProjectUnitType,
    pub is_deleted: bool,
    pub context_slug: String,
    pub module_slug: String,
    pub unit_name_slug: String,
    pub metadata: Option<UnitMetadata>,
    pub depends_on: Option<String>,
}

#[derive(Deserialize)]
struct ProjectUnitResponseDe {
    #[serde(flatten)]
    row: ProjectUnitRow,
    #[serde(default)]
    latest_revision: Option<i32>,
    #[serde(default)]
    is_draft: Option<bool>,
    #[serde(default)]
    is_rejected: bool,
}

impl From<ProjectUnitResponseDe> for ProjectUnitResponse {
    fn from(d: ProjectUnitResponseDe) -> Self {
        let latest_revision = d.latest_revision.unwrap_or(d.row.revision);
        Self {
            row: d.row,
            latest_revision,
            is_draft: d.is_draft,
            is_rejected: d.is_rejected,
        }
    }
}

/// Unit payload for APIs, chat messages, and LLM tool results. `latest_revision` is the maximum
/// `revision` for this logical unit (same project, session, and slug tuple), for UI like `revision N/M`.
///
/// Persisted chat message JSON omits `latest_revision` (redundant at save time); use
/// `POST .../chat-sessions/{session_id}/unit-tips-by-ids` to resolve current tips when rendering history,
/// or `POST .../units-by-ids` to keep the stored revision row but fill `latest_revision`.
/// `POST .../unit-latest-revisions-by-ids` returns only the max `revision` per unit path without loading full rows.
/// `POST .../unit-pre-chat-base-by-paths` returns the row per path from the session’s baseline production release combined snapshot.
/// `POST .../chat-sessions/.../unit-previous-by-full-ids` returns the prior revision per full unit id (session row first, else baseline snapshot); the current id need not still exist (e.g. after reject). **404** only when `revision > 1` and that prior revision is missing from both places.
///
/// JSON is a single flat object (`row` fields + `latest_revision`, optional `is_draft`, optional `is_rejected` on
/// reject-controller `unit_changes`). Use [`Deref`] to access row fields as `unit.revision`, `unit.context_slug`, etc.
///
/// `is_draft` is set on project **units** HTTP responses: `false` when this row’s `revision` matches
/// the latest production combined snapshot for the same logical unit (`context_slug`, `module_slug`,
/// `unit_type`, `unit_name_slug`); otherwise `true`. Chat/tool payloads may omit it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(from = "ProjectUnitResponseDe")]
pub struct ProjectUnitResponse {
    #[serde(flatten)]
    pub row: ProjectUnitRow,
    pub latest_revision: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_rejected: bool,
}

impl ProjectUnitResponse {
    pub fn from_row(row: ProjectUnitRow, latest_revision: i32) -> Self {
        Self {
            row,
            latest_revision,
            is_draft: None,
            is_rejected: false,
        }
    }
}

impl Deref for ProjectUnitResponse {
    type Target = ProjectUnitRow;

    fn deref(&self) -> &Self::Target {
        &self.row
    }
}

#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_runtimes)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub environment: PlatformEnvironmentType,
    /// [`crate::schema::project_releases::id`] when this runtime has a live release.
    pub live_release_id: Option<Uuid>,
}

/// Runtime row plus the joined [`ProjectReleaseResponse`] for [`crate::schema::project_runtimes::live_release_id`], if set.
#[derive(Serialize, Queryable, Selectable)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeWithLiveReleaseResponse {
    #[diesel(embed)]
    #[serde(flatten)]
    pub runtime: ProjectRuntimeResponse,
    #[diesel(embed)]
    pub live_release: Option<ProjectReleaseResponse>,
}

/// API output for [`ProjectRuntimeWithLiveReleaseResponse`]: `live_release` includes `base_production_release`.
#[derive(Serialize)]
pub struct ProjectRuntimeWithLiveReleaseApi {
    #[serde(flatten)]
    pub runtime: ProjectRuntimeResponse,
    pub live_release: Option<ProjectReleaseWithBaseline>,
}

#[derive(Serialize)]
pub struct LiveReleaseByRevisionApiResponse {
    pub runtimes: Vec<ProjectRuntimeWithLiveReleaseApi>,
}

/// One unit pointer in [`ReleaseUnitsMetadata`]: `unit_id` is the full stored id (path + revision).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseUnitRef {
    pub chat_session_id: Uuid,
    pub unit_id: String,
}

/// JSON stored in `project_releases.units_metadata` for stage/production releases from this API.
/// `previousUnitIDs` is the full combined snapshot **before** this release; `POST .../releases/{release_id}/unit-previous-by-full-ids` resolves parents of `newUnitIDs` rows by matching the prior revision id inside that snapshot (see [`crate::services::releases::load_unit_previous_revisions_for_release`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseUnitsMetadata {
    #[serde(rename = "previousUnitIDs")]
    pub previous_unit_refs: Vec<ReleaseUnitRef>,
    #[serde(rename = "newUnitIDs")]
    pub new_unit_refs: Vec<ReleaseUnitRef>,
    #[serde(rename = "combinedUnitIDs")]
    pub combined_unit_refs: Vec<ReleaseUnitRef>,
}

impl FromSql<Jsonb, Pg> for ReleaseUnitsMetadata {
    fn from_sql(value: diesel::pg::PgValue<'_>) -> deserialize::Result<Self> {
        let v = <serde_json::Value as FromSql<Jsonb, Pg>>::from_sql(value)?;
        serde_json::from_value(v).map_err(|e| Box::new(e) as _)
    }
}

impl ToSql<Jsonb, Pg> for ReleaseUnitsMetadata {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        // Match `serde_json::Value`’s Jsonb encoding: version prefix + JSON bytes.
        out.write_all(&[1])?;
        serde_json::to_writer(out, self)
            .map(|_| IsNull::No)
            .map_err(Into::into)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_releases)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectReleaseResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    /// [`ReleaseUnitsMetadata`] (`previousUnitIDs` / `newUnitIDs` / `combinedUnitIDs`).
    pub units_metadata: Option<ReleaseUnitsMetadata>,
    pub created_at: DateTime<Utc>,
    pub notes: Option<String>,
    /// Short Markdown summary (~3 lines) of the same diff as `notes`, for list UI and tooltips.
    pub brief_note: Option<String>,
    pub chat_session_id: Option<Uuid>,
    pub is_stage: bool,
    pub revision: f64,
    /// Production release that was live when this chat session was created; all stage/prod rows
    /// created in-session share this baseline.
    pub base_production_release_id: Option<Uuid>,
}

/// Release row as returned to clients: same fields as [`ProjectReleaseResponse`] plus the resolved
/// baseline production row when `base_production_release_id` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectReleaseWithBaseline {
    #[serde(flatten)]
    pub release: ProjectReleaseResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_production_release: Option<ProjectReleaseResponse>,
}

impl From<ProjectReleaseResponse> for ProjectReleaseWithBaseline {
    fn from(release: ProjectReleaseResponse) -> Self {
        Self {
            release,
            base_production_release: None,
        }
    }
}
