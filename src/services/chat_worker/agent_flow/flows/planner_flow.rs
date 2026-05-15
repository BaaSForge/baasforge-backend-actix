use crate::models::chat_messages::ChatAgentType;
use crate::services::chat_worker::agent::tool_names_for_agent;
use crate::services::chat_worker::agent_flow::context::SessionFlowContext;
use crate::services::chat_worker::agent_flow::engine::{FlowDefinition, FlowStep};
use crate::services::chat_worker::llm::tools::{
    analyze_architecture_tool, analyze_observability_tool, analyze_solution_tool,
    load_latest_releases,
};

const FLOW_ID: &str = "planner_flow";

const STEP_DISCOVER: &str = "discover";
const STEP_PLAN_SOLUTION: &str = "plan_solution";
const STEP_IMPLEMENT: &str = "implement";

fn is_discover(ctx: &SessionFlowContext) -> bool {
    ctx.is_pre_architecture_phase()
}

fn is_plan_solution(ctx: &SessionFlowContext) -> bool {
    ctx.can_access_solution() && !ctx.can_access_implement()
}

fn is_implement(ctx: &SessionFlowContext) -> bool {
    ctx.can_access_implement()
}

fn tools_discover(_ctx: &SessionFlowContext) -> Vec<&'static str> {
    vec![
        analyze_architecture_tool::TOOL_NAME,
        analyze_observability_tool::TOOL_NAME,
        load_latest_releases::TOOL_NAME,
    ]
}

fn tools_plan_solution(_ctx: &SessionFlowContext) -> Vec<&'static str> {
    vec![
        analyze_architecture_tool::TOOL_NAME,
        analyze_observability_tool::TOOL_NAME,
        analyze_solution_tool::TOOL_NAME,
        load_latest_releases::TOOL_NAME,
    ]
}

fn tools_implement(_ctx: &SessionFlowContext) -> Vec<&'static str> {
    tool_names_for_agent(&ChatAgentType::Planner).to_vec()
}

fn prompt_discover(_ctx: &SessionFlowContext) -> Option<String> {
    Some(
        "## Workflow step: Discover (system)\n\
        You are in the **discovery** phase.
        - **Analyze solution** and **implement solution** are **not** available until you have \
        run architecture analysis at least once in this session."
            .to_string(),
    )
}

fn prompt_plan_solution(_ctx: &SessionFlowContext) -> Option<String> {
    Some(
        "## Workflow step: Plan solution (system)\n\
        Architecture analysis has been run in this session. You may now use **analyze solution** \
        (or architecture analysis, observability, and load latest releases). **Implement solution** is \
        still locked until a solution analysis has completed.\n\
        - Do not skip solution analysis before implementation.\n"
            .to_string(),
    )
}

fn prompt_implement(ctx: &SessionFlowContext) -> Option<String> {
    let pending_impl = ctx.has_solution_without_following_implement();
    Some(format!(
        "## Workflow step: Implement (system)\n\
        Solution analysis has been run; **implement solution** is now available.\n\
        - Solution analysis without a following implement call: **{pending_impl}**."
    ))
}

pub static PLANNER_FLOW: FlowDefinition<SessionFlowContext> = FlowDefinition {
    id: FLOW_ID,
    steps: &[
        FlowStep {
            id: STEP_IMPLEMENT,
            when: is_implement,
            allowed_tools: tools_implement,
            prompt_appendix: prompt_implement,
        },
        FlowStep {
            id: STEP_PLAN_SOLUTION,
            when: is_plan_solution,
            allowed_tools: tools_plan_solution,
            prompt_appendix: prompt_plan_solution,
        },
        FlowStep {
            id: STEP_DISCOVER,
            when: is_discover,
            allowed_tools: tools_discover,
            prompt_appendix: prompt_discover,
        },
    ],
    fallback: FlowStep {
        id: STEP_DISCOVER,
        when: |_| true,
        allowed_tools: tools_discover,
        prompt_appendix: prompt_discover,
    },
};
