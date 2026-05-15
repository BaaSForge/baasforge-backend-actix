use crate::services::chat_worker::agent_flow::{
    SubAgentBranchContext, DISCOVER_UNITS_TRIO,
};
use crate::services::chat_worker::agent_flow::engine::{FlowDefinition, FlowStep};
use crate::services::chat_worker::llm::tools::{
    finish_architecture_analysis, list_modules_for_unit_contexts, list_unit_contexts,
    load_latest_production_release_units,
};

const FLOW_ID: &str = "architecture_analyzer_gate";

fn gate_prompt(ctx: &SubAgentBranchContext) -> Option<String> {
    if ctx.all_invoked(DISCOVER_UNITS_TRIO) {
        return None;
    }
    Some(
        "## Tool gate (system)\n\
        **finish_architecture_analysis** is available only after you have called **load_latest_production_release_units**, \
        **list_unit_contexts**, and **list_modules_for_unit_contexts** at least once each in this nested run."
            .to_string(),
    )
}

pub static ARCHITECTURE_ANALYZER_FLOW: FlowDefinition<SubAgentBranchContext> = FlowDefinition {
    id: FLOW_ID,
    steps: &[FlowStep {
        id: "ready_to_finish",
        when: |ctx| ctx.all_invoked(DISCOVER_UNITS_TRIO),
        allowed_tools: |_| {
            vec![
                load_latest_production_release_units::TOOL_NAME,
                list_unit_contexts::TOOL_NAME,
                list_modules_for_unit_contexts::TOOL_NAME,
                finish_architecture_analysis::TOOL_NAME,
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
