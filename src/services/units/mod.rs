pub mod actor;
pub mod api;
pub mod common;
pub mod queue_worker;
pub mod runtime_dependencies;
pub mod type_extractor;
pub mod typescript_json_schema;
pub mod workflow;

use serde::{Deserialize, Serialize};

pub use api::metadata::HttpMethod;

/// Per-unit-type metadata stored as JSONB in `project_units.metadata`.
/// Tagged with `unit_type` so each variant deserializes unambiguously.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, diesel::AsExpression, diesel::FromSqlRow,
)]
#[diesel(sql_type = diesel::sql_types::Jsonb)]
#[serde(tag = "unit_type", rename_all = "snake_case")]
pub enum UnitMetadata {
    Api(api::metadata::ApiUnitMetadata),
    Workflow(workflow::metadata::WorkflowUnitMetadata),
    QueueWorker(queue_worker::metadata::QueueWorkerUnitMetadata),
    Actor(actor::metadata::ActorUnitMetadata),
}

/// Snake-case ids for each unit kind. Same order as [`all_briefs`] / [`all_details`] / [`all_runtimes`] and
/// [`crate::models::projects::ProjectUnitType`] (serde snake_case). Keep in sync when adding a unit definition module.
macro_rules! define_unit_kind_slugs {
    ($first:literal $(, $rest:literal)*) => {
        pub const UNIT_KIND_SLUGS: &[&str] = &[$first $(, $rest)*];

        /// For `#[schemars(description = ...)]` on create_unit's `unit_type` field (Gemini JSON schema).
        pub const UNIT_TYPE_ARG_DESCRIPTION: &str = concat!(
            "Unit kind (enum): pass the lowercase slug string exactly as in the schema—e.g. api, workflow—not Title Case (Api) or SCREAMING_SNAKE (API). Must be one of: ",
            $first $(, ", ", $rest)*
        );
    };
}

define_unit_kind_slugs!("api", "workflow", "queue_worker", "actor", "common");

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
        (workflow::TITLE, workflow::BRIEF),
        (queue_worker::TITLE, queue_worker::BRIEF),
        (actor::TITLE, actor::BRIEF),
        (common::TITLE, common::BRIEF),
    ];
    ROWS.iter()
        .map(|(title, brief)| format!("### {title}\n\n{brief}\n"))
        .collect()
}

pub fn all_details() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::DETAIL),
        (workflow::TITLE, workflow::DETAIL),
        (queue_worker::TITLE, queue_worker::DETAIL),
        (actor::TITLE, actor::DETAIL),
        (common::TITLE, common::DETAIL),
    ];
    ROWS.iter()
        .map(|(title, detail)| format!("### {title}\n\n{detail}\n"))
        .collect()
}

pub fn all_code_examples() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::CODE_EXAMPLES),
        (workflow::TITLE, workflow::CODE_EXAMPLES),
        (queue_worker::TITLE, queue_worker::CODE_EXAMPLES),
        (actor::TITLE, actor::CODE_EXAMPLES),
        (common::TITLE, common::CODE_EXAMPLES),
    ];
    ROWS.iter()
        .map(|(title, example)| format!("### {title}\n\n{example}\n"))
        .collect()
}

pub fn all_communications() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::COMMUNICATION),
        (workflow::TITLE, workflow::COMMUNICATION),
        (queue_worker::TITLE, queue_worker::COMMUNICATION),
        (actor::TITLE, actor::COMMUNICATION),
        (common::TITLE, common::COMMUNICATION),
    ];
    ROWS.iter()
        .map(|(title, comm)| format!("### {title}\n\n{comm}\n"))
        .collect()
}

// Prefixed to [`all_runtimes`] output once (before per-unit `RUNTIME` blocks).
const RUNTIME_PROMPT_EXPORTED_TYPES_PITFALL: &str = r#"### Pitfall — dashboard endpoints follow exported types only

Endpoint documentation and the HTTP client in the dashboard are **generated automatically** from the **types exported in the unit code**. If those exports are wrong or do not match the real unit definition (handlers, payloads and ...), the generated API surface is wrong too, because it **only** relies on those exported types—so it is important they **match the actual unit code**.

**Critical:** Those auto-generated endpoints in the dashboard and its documentation **are not editable by the user**. They always follow the types exported in the unit code. If the unit exports wrong types, the generated documentation and try-it flows will **keep** invoking the unit the wrong way, and the user **cannot** fix that in the dashboard UI alone—it stays automatic from exports until the unit source exports the correct types.

"#;

/// Per-unit `RUNTIME` prompt text for each kind, prefixed once with a shared pitfall: dashboard
/// HTTP docs and try-it clients are generated from exported types only and are not user-editable.
pub fn all_runtimes() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::RUNTIME),
        (workflow::TITLE, workflow::RUNTIME),
        (queue_worker::TITLE, queue_worker::RUNTIME),
        (actor::TITLE, actor::RUNTIME),
        (common::TITLE, common::RUNTIME),
    ];
    let per_kind: String = ROWS
        .iter()
        .map(|(title, runtime)| format!("### {title}\n\n{runtime}\n"))
        .collect();
    format!("{RUNTIME_PROMPT_EXPORTED_TYPES_PITFALL}{per_kind}")
}

pub fn all_metadata() -> String {
    const ROWS: &[(&str, &str)] = &[
        (api::TITLE, api::METADATA),
        (workflow::TITLE, workflow::METADATA),
        (queue_worker::TITLE, queue_worker::METADATA),
        (actor::TITLE, actor::METADATA),
        (common::TITLE, common::METADATA),
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
    /// Request handling timeout in seconds — relevant only for `Api` units.
    pub api_timeout_seconds: Option<u32>,
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
                input.api_timeout_seconds,
            )
            .await
        }
        crate::models::projects::ProjectUnitType::Workflow => {
            workflow::build_unit_metadata(pool, project_id, session_id, input.code).await
        }
        crate::models::projects::ProjectUnitType::QueueWorker => {
            queue_worker::build_unit_metadata(pool, project_id, session_id, input.code).await
        }
        crate::models::projects::ProjectUnitType::Actor => {
            actor::build_unit_metadata(pool, project_id, session_id, input.code).await
        }
        crate::models::projects::ProjectUnitType::Common => None,
    }
}
