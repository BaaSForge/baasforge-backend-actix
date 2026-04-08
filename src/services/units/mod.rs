mod action;
pub mod api;
mod bootstrap;
mod command;
mod flow;
mod gateway;
mod projection;
mod query;
mod runtime;
mod schema;
mod timer;
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
    Action(action::metadata::ActionUnitMetadata),
    Bootstrap(bootstrap::metadata::BootstrapUnitMetadata),
    Command(command::metadata::CommandUnitMetadata),
    Flow(flow::metadata::FlowUnitMetadata),
    Gateway(gateway::metadata::GatewayUnitMetadata),
    Projection(projection::metadata::ProjectionUnitMetadata),
    Query(query::metadata::QueryUnitMetadata),
    Runtime(runtime::metadata::RuntimeUnitMetadata),
    Schema(schema::metadata::SchemaUnitMetadata),
    Timer(timer::metadata::TimerUnitMetadata),
}

/// Snake-case ids for each unit kind. Same order as [`all_briefs`] / [`all_details`] and
/// [`crate::models::projects::ProjectUnitType`] (serde snake_case). Keep in sync when adding a unit definition module.
pub const UNIT_KIND_SLUGS: &[&str] = &[
    "bootstrap",
    "action",
    "api",
    "command",
    "flow",
    "gateway",
    "projection",
    "query",
    "runtime",
    "schema",
    "timer",
];

/// For `#[schemars(description = ...)]` on create_unit's `unit_type` field (Gemini JSON schema).
pub const UNIT_TYPE_ARG_DESCRIPTION: &str =
    "Unit kind (enum): pass the lowercase slug string exactly as in the schema—e.g. api, command, projection—not Title Case (Api) or SCREAMING_SNAKE (API). Must be one of: bootstrap, action, api, command, flow, gateway, projection, query, runtime, schema, timer.";

/// Prose for LLM tool parameters (`unit_type` on `create_unit` JSON schema `description`).
pub fn unit_type_description_for_tools() -> String {
    format!(
        "Unit kind (enum). Use the lowercase slug from this exact set: {}.",
        UNIT_KIND_SLUGS.join(", ")
    )
}

pub fn all_briefs() -> String {
    const ROWS: &[(&str, &str)] = &[
        (bootstrap::TITLE, bootstrap::BRIEF),
        (action::TITLE, action::BRIEF),
        (api::TITLE, api::BRIEF),
        (command::TITLE, command::BRIEF),
        (flow::TITLE, flow::BRIEF),
        (gateway::TITLE, gateway::BRIEF),
        (projection::TITLE, projection::BRIEF),
        (query::TITLE, query::BRIEF),
        (runtime::TITLE, runtime::BRIEF),
        (schema::TITLE, schema::BRIEF),
        (timer::TITLE, timer::BRIEF),
    ];
    ROWS.iter()
        .map(|(title, brief)| format!("### {title}\n\n{brief}\n"))
        .collect()
}

pub fn all_details() -> String {
    const ROWS: &[(&str, &str)] = &[
        (bootstrap::TITLE, bootstrap::DETAIL),
        (action::TITLE, action::DETAIL),
        (api::TITLE, api::DETAIL),
        (command::TITLE, command::DETAIL),
        (flow::TITLE, flow::DETAIL),
        (gateway::TITLE, gateway::DETAIL),
        (projection::TITLE, projection::DETAIL),
        (query::TITLE, query::DETAIL),
        (runtime::TITLE, runtime::DETAIL),
        (schema::TITLE, schema::DETAIL),
        (timer::TITLE, timer::DETAIL),
    ];
    ROWS.iter()
        .map(|(title, detail)| format!("### {title}\n\n{detail}\n"))
        .collect()
}

pub fn all_communications() -> String {
    const ROWS: &[(&str, &str)] = &[
        (bootstrap::TITLE, bootstrap::COMMUNICATION),
        (action::TITLE, action::COMMUNICATION),
        (api::TITLE, api::COMMUNICATION),
        (command::TITLE, command::COMMUNICATION),
        (flow::TITLE, flow::COMMUNICATION),
        (gateway::TITLE, gateway::COMMUNICATION),
        (projection::TITLE, projection::COMMUNICATION),
        (query::TITLE, query::COMMUNICATION),
        (runtime::TITLE, runtime::COMMUNICATION),
        (schema::TITLE, schema::COMMUNICATION),
        (timer::TITLE, timer::COMMUNICATION),
    ];
    ROWS.iter()
        .map(|(title, comm)| format!("### {title}\n\n{comm}\n"))
        .collect()
}

pub fn all_metadata() -> String {
    const ROWS: &[(&str, &str)] = &[
        (bootstrap::TITLE, bootstrap::METADATA),
        (action::TITLE, action::METADATA),
        (api::TITLE, api::METADATA),
        (command::TITLE, command::METADATA),
        (flow::TITLE, flow::METADATA),
        (gateway::TITLE, gateway::METADATA),
        (projection::TITLE, projection::METADATA),
        (query::TITLE, query::METADATA),
        (runtime::TITLE, runtime::METADATA),
        (schema::TITLE, schema::METADATA),
        (timer::TITLE, timer::METADATA),
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
