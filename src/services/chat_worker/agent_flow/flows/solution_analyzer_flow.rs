use crate::services::chat_worker::agent_flow::SubAgentBranchContext;
use crate::services::chat_worker::agent_flow::engine::{FlowDefinition, FlowStep};
use crate::services::chat_worker::llm::tools::{
    finish_solution_analysis, list_modules_for_unit_contexts, list_unit_contexts,
    load_latest_production_release_units,
};

const FLOW_ID: &str = "solution_analyzer_gate";

fn gate_prompt(ctx: &SubAgentBranchContext) -> Option<String> {
    if ctx.discover_units_prerequisites_met() {
        return None;
    }
    Some(
        "## Tool gate (system)\n\
        Before **finish_solution_analysis** unlocks, you must call **load_latest_production_release_units** once \
        and **list_unit_contexts** once in this run. If **list_unit_contexts** returned at least one context slug, \
        you must also call **list_modules_for_unit_contexts** once before unlock. If it returned an empty list of context slugs, \
        **list_modules_for_unit_contexts** is not mandatory for unlock."
            .to_string(),
    )
}

pub static SOLUTION_ANALYZER_FLOW: FlowDefinition<SubAgentBranchContext> = FlowDefinition {
    id: FLOW_ID,
    steps: &[FlowStep {
        id: "ready_to_finish",
        when: |ctx| ctx.discover_units_prerequisites_met(),
        allowed_tools: |_| {
            vec![
                load_latest_production_release_units::TOOL_NAME,
                list_unit_contexts::TOOL_NAME,
                list_modules_for_unit_contexts::TOOL_NAME,
                finish_solution_analysis::TOOL_NAME,
            ]
        },
        prompt_appendix: gate_prompt,
    }],
    fallback: FlowStep {
        id: "discover_units",
        when: |_| true,
        allowed_tools: |_| {
            vec![
                load_latest_production_release_units::TOOL_NAME,
                list_unit_contexts::TOOL_NAME,
                list_modules_for_unit_contexts::TOOL_NAME,
            ]
        },
        prompt_appendix: gate_prompt,
    },
};
