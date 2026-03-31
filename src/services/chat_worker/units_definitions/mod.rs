mod action;
mod api;
mod bootstrap;
mod command;
mod flow;
mod gateway;
mod projection;
mod query;
mod runtime;
mod schema;
mod timer;

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

/// For `#[schemars(description = ...)]` on create_unit’s `unit_type` field (Gemini JSON schema).
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
    ROWS
        .iter()
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
    ROWS
        .iter()
        .map(|(title, detail)| format!("### {title}\n\n{detail}\n"))
        .collect()
}
