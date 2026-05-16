use crate::services::chat_worker::agent_flow::SubAgentBranchContext;
use crate::services::chat_worker::agent_flow::discover_units_gate;
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
    Some(discover_units_gate::gate_appendix(
        &discover_units_gate::solution_finish_locked(),
    ))
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
