//! Shared definition of **unit-discovery prerequisites** for nested agents (architecture / solution
//! analyzers, programmer). Used in flow prompt appendixes and tool-lock errors so the model sees the
//! same meaning whether or not the appendix is still injected.

use crate::services::chat_worker::llm::tools::{
    create_unit, finish_architecture_analysis, finish_implementation, finish_solution_analysis,
    list_modules_for_unit_contexts, list_unit_contexts, load_latest_production_release_units,
    load_latest_specific_production_release_units, modify_unit, remove_unit,
};

/// What “unit-discovery prerequisites” means — keep in sync with
/// [`crate::services::chat_worker::agent_flow::history_scan::FlowHistoryScan::discover_units_prerequisites_met`].
pub fn unit_discovery_prerequisites_definition() -> String {
    format!(
        "Unit-discovery prerequisites (this nested run): call `{load}` once and `{list_ctx}` once; \
if `{list_ctx}` returned a non-empty `context_slugs` list, also call `{list_mod}` once \
(not required when `context_slugs` was empty).",
        load = load_latest_production_release_units::TOOL_NAME,
        list_ctx = list_unit_contexts::TOOL_NAME,
        list_mod = list_modules_for_unit_contexts::TOOL_NAME,
    )
}

pub fn gate_appendix(locked_until_prerequisites: &str) -> String {
    format!(
        "## Tool gate (system)\n\
        {}\n\n\
        {locked_until_prerequisites}",
        unit_discovery_prerequisites_definition()
    )
}

pub fn architecture_finish_locked() -> String {
    format!(
        "Until they are met, `{finish}` stays locked.",
        finish = finish_architecture_analysis::TOOL_NAME
    )
}

pub fn solution_finish_locked() -> String {
    format!(
        "Until they are met, `{finish}` stays locked.",
        finish = finish_solution_analysis::TOOL_NAME
    )
}

pub fn developer_mutate_load_prerequisite_definition() -> String {
    format!(
        "Before `{modify}` or `{remove}`, call `{load}` successfully in this run with that unit's path \
(`context.module.unit_type.unit_name`, four segments) in `unit_paths` so you have read the current source.",
        modify = modify_unit::TOOL_NAME,
        remove = remove_unit::TOOL_NAME,
        load = load_latest_specific_production_release_units::TOOL_NAME,
    )
}

pub fn developer_mutate_finish_locked() -> String {
    format!(
        "Until unit-discovery prerequisites are met, `{create}`, `{modify}`, `{remove}`, and `{finish}` stay locked. \
After discovery, {load_prereq}",
        create = create_unit::TOOL_NAME,
        modify = modify_unit::TOOL_NAME,
        remove = remove_unit::TOOL_NAME,
        finish = finish_implementation::TOOL_NAME,
        load_prereq = developer_mutate_load_prerequisite_definition(),
    )
}

pub fn mutate_unit_locked_until_load_message(tool_name: &str, unit_path: &str) -> String {
    format!(
        "`{tool_name}` is locked for unit path `{unit_path}` until you complete a successful `{load}` call \
in this run that includes `{unit_path}` in `unit_paths`. {detail}",
        load = load_latest_specific_production_release_units::TOOL_NAME,
        detail = developer_mutate_load_prerequisite_definition(),
    )
}

pub fn lock_error_preamble(tool_name: &str) -> String {
    format!(
        "`{tool_name}` is locked until unit-discovery prerequisites complete in this nested run. {}",
        unit_discovery_prerequisites_definition()
    )
}
