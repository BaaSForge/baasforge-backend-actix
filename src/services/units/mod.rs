pub mod api;
pub mod command;
pub mod projection;
pub mod query;
pub mod saga;
pub mod type_extractor;

use serde::{Deserialize, Serialize};

pub use api::metadata::HttpMethod;

/// Per-unit-type metadata stored as JSONB in `project_units.metadata`.
/// Tagged with `unit_type` so each variant deserializes unambiguously.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, diesel::AsExpression, diesel::FromSqlRow)]
#[diesel(sql_type = diesel::sql_types::Jsonb)]
#[serde(tag = "unit_type", rename_all = "snake_case")]
pub enum UnitMetadata {
    Api(api::metadata::ApiUnitMetadata),
    Command(command::metadata::CommandUnitMetadata),
    Saga(saga::metadata::SagaUnitMetadata),
    Projection(projection::metadata::ProjectionUnitMetadata),
    Query(query::metadata::QueryUnitMetadata),
}

/// Snake-case ids for each unit kind. Same order as [`all_briefs`] / [`all_details`] and
/// [`crate::models::projects::ProjectUnitType`] (serde snake_case). Keep in sync when adding a unit definition module.
macro_rules! define_unit_kind_slugs {
    ($first:literal $(, $rest:literal)*) => {
        pub const UNIT_KIND_SLUGS: &[&str] = &[$first $(, $rest)*];

        /// For `#[schemars(description = ...)]` on create_unit's `unit_type` field (Gemini JSON schema).
        pub const UNIT_TYPE_ARG_DESCRIPTION: &str = concat!(
            "Unit kind (enum): pass the lowercase slug string exactly as in the schema—e.g. api, command, projection—not Title Case (Api) or SCREAMING_SNAKE (API). Must be one of: ",
            $first $(, ", ", $rest)*
        );
    };
}

define_unit_kind_slugs!(
    "api",
    "command",
    "saga",
    "gateway",
    "projection",
    "query",
    "policy"
);

/// Prose for LLM tool parameters (`unit_type` on `create_unit` JSON schema `description`).
pub fn unit_type_description_for_tools() -> String {
    format!(
        "Unit kind (enum). Use the lowercase slug from this exact set: {}.",
        UNIT_KIND_SLUGS.join(", ")
    )
}

pub fn all_briefs() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::BRIEF),
        (command::TITLE, command::BRIEF),
        (saga::TITLE, saga::BRIEF),
        (projection::TITLE, projection::BRIEF),
        (query::TITLE, query::BRIEF),
    ];
    ROWS.iter()
        .map(|(title, brief)| format!("### {title}\n\n{brief}\n"))
        .collect()
}

pub fn all_details() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::DETAIL),
        (command::TITLE, command::DETAIL),
        (saga::TITLE, saga::DETAIL),
        (projection::TITLE, projection::DETAIL),
        (query::TITLE, query::DETAIL),
    ];
    ROWS.iter()
        .map(|(title, detail)| format!("### {title}\n\n{detail}\n"))
        .collect()
}

pub fn all_communications() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::COMMUNICATION),
        (command::TITLE, command::COMMUNICATION),
        (saga::TITLE, saga::COMMUNICATION),
        (projection::TITLE, projection::COMMUNICATION),
        (query::TITLE, query::COMMUNICATION),
    ];
    ROWS.iter()
        .map(|(title, comm)| format!("### {title}\n\n{comm}\n"))
        .collect()
}

pub fn all_metadata() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::METADATA),
        (command::TITLE, command::METADATA),
        (saga::TITLE, saga::METADATA),
        (projection::TITLE, projection::METADATA),
        (query::TITLE, query::METADATA),
    ];
    ROWS.iter()
        .map(|(title, meta)| format!("### {title}\n\n{meta}\n"))
        .collect()
}

// ── Metadata building ─────────────────────────────────────────────────────────

/// Unit-type–agnostic input for `build_unit_metadata`.
///
/// Callers populate all fields from the LLM tool payload; each unit type uses
/// only the fields relevant to it.
pub struct UnitMetadataBuildInput<'a> {
    pub code: &'a str,
    /// HTTP method — relevant only for `Api` units.
    pub api_method: Option<HttpMethod>,
}

/// Build the persisted `UnitMetadata` for a unit after all checks pass.
///
/// This is the single dispatch point: each unit type is responsible for its own
/// `build_unit_metadata` implementation in its module folder; the match here is
/// the only place in the technical-reviewer pipeline that is aware of concrete
/// unit types.
pub async fn build_unit_metadata(
    unit_type: crate::models::projects::ProjectUnitType,
    pool: &crate::database::DbPool,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    input: UnitMetadataBuildInput<'_>,
) -> Option<UnitMetadata> {
    match unit_type {
        crate::models::projects::ProjectUnitType::Api => {
            api::build_unit_metadata(
                pool,
                project_id,
                session_id,
                input.code,
                input.api_method,
            )
            .await
        }
        _ => None,
    }
}
