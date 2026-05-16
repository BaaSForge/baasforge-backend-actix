use crate::services::chat_worker::agent_flow::SubAgentBranchContext;
use crate::services::chat_worker::agent_flow::engine::{FlowDefinition, FlowStep};
use crate::services::chat_worker::llm::tools::{
    fetch_release_traces, finish_observability_analysis, list_project_env_keys,
    load_latest_production_release_units,
};

const FLOW_ID: &str = "observability_analyzer_gate";

fn prompt_trace_only(ctx: &SubAgentBranchContext) -> Option<String> {
    if ctx.ever_invoked(fetch_release_traces::TOOL_NAME) {
        return None;
    }
    Some(
        "## Tool gate (system)\n\
        Only **fetch_release_traces** is available until you have invoked it at least once. \
        Then **load_latest_production_release_units** unlocks."
            .to_string(),
    )
}

fn prompt_no_finish(ctx: &SubAgentBranchContext) -> Option<String> {
    if ctx.ever_invoked(load_latest_production_release_units::TOOL_NAME) {
        return None;
    }
    Some(
        "## Tool gate (system)\n\
        **finish_observability_analysis** unlocks only after both **fetch_release_traces** and \
        **load_latest_production_release_units** have been called in this run."
            .to_string(),
    )
}

pub static OBSERVABILITY_ANALYZER_FLOW: FlowDefinition<SubAgentBranchContext> = FlowDefinition {
    id: FLOW_ID,
    steps: &[
        FlowStep {
            id: "ready_to_finish",
            when: |ctx| {
                ctx.ever_invoked(fetch_release_traces::TOOL_NAME)
                    && ctx.ever_invoked(load_latest_production_release_units::TOOL_NAME)
            },
            allowed_tools: |_| {
                vec![
                    fetch_release_traces::TOOL_NAME,
                    load_latest_production_release_units::TOOL_NAME,
                    list_project_env_keys::TOOL_NAME,
                    finish_observability_analysis::TOOL_NAME,
                ]
            },
            prompt_appendix: |_| None,
        },
        FlowStep {
            id: "after_traces",
            when: |ctx| ctx.ever_invoked(fetch_release_traces::TOOL_NAME),
            allowed_tools: |_| {
                vec![
                    fetch_release_traces::TOOL_NAME,
                    load_latest_production_release_units::TOOL_NAME,
                    list_project_env_keys::TOOL_NAME,
                ]
            },
            prompt_appendix: prompt_no_finish,
        },
    ],
    fallback: FlowStep {
        id: "traces_first",
        when: |_| true,
        allowed_tools: |_| {
            vec![
                fetch_release_traces::TOOL_NAME,
                list_project_env_keys::TOOL_NAME,
            ]
        },
        prompt_appendix: prompt_trace_only,
    },
};
