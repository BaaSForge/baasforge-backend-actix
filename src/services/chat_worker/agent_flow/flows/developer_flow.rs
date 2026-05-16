use crate::models::chat_messages::ChatAgentType;
use crate::services::chat_worker::agent::tool_names_for_agent;
use crate::services::chat_worker::agent_flow::SubAgentBranchContext;
use crate::services::chat_worker::agent_flow::discover_units_gate;
use crate::services::chat_worker::agent_flow::engine::{FlowDefinition, FlowStep};
use crate::services::chat_worker::llm::tools::{
    list_modules_for_unit_contexts, list_project_env_keys, list_unit_contexts,
    load_latest_production_release_units,
};

const FLOW_ID: &str = "developer_gate";

fn gate_prompt(ctx: &SubAgentBranchContext) -> Option<String> {
    if ctx.discover_units_prerequisites_met() {
        return None;
    }
    Some(discover_units_gate::gate_appendix(
        &discover_units_gate::developer_mutate_finish_locked(),
    ))
}

pub static DEVELOPER_FLOW: FlowDefinition<SubAgentBranchContext> = FlowDefinition {
    id: FLOW_ID,
    steps: &[FlowStep {
        id: "ready_to_mutate",
        when: |ctx| ctx.discover_units_prerequisites_met(),
        allowed_tools: |_| tool_names_for_agent(&ChatAgentType::Developer).to_vec(),
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
                list_project_env_keys::TOOL_NAME,
            ]
        },
        prompt_appendix: gate_prompt,
    },
};
