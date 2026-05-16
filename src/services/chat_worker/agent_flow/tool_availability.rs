use crate::models::chat_messages::{ChatAgentType, ChatMessageType, ChatSessionRoleType};
use crate::services::chat_worker::agent::{agent_prompt_role_name, tool_names_for_agent};
use crate::services::chat_worker::agent_flow::context::SessionFlowContext;
use crate::services::chat_worker::agent_flow::discover_units_gate;
use crate::services::chat_worker::agent_flow::history_scan::FlowHistoryScan;
use crate::services::chat_worker::agent_flow::AgentFlowSession;
use crate::services::chat_worker::llm::tools::{
    self, analyze_architecture_tool, analyze_observability_tool, analyze_solution_tool,
    create_unit, fetch_release_traces, finish_architecture_analysis, finish_implementation,
    finish_observability_analysis, finish_solution_analysis, implement_solution_tool,
    list_modules_for_unit_contexts, list_unit_contexts, load_latest_production_release_units,
    load_latest_releases, modify_unit, remove_unit,
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ToolGateDiagnostics {
    agent: ChatAgentType,
    flow_id: Option<&'static str>,
    step_id: Option<&'static str>,
    allowed_tools: Vec<&'static str>,
    planner: Option<SessionFlowContext>,
    sub_agent_scan: Option<FlowHistoryScan>,
}

impl ToolGateDiagnostics {
    pub fn build(
        agent: ChatAgentType,
        messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
        flow: &AgentFlowSession,
    ) -> Self {
        let resolution = flow.resolution.as_ref();
        let (flow_id, step_id, allowed_tools) = match resolution {
            Some(r) => (
                Some(r.flow_id),
                Some(r.step_id),
                r.allowed_tool_names.clone(),
            ),
            None => (None, None, Vec::new()),
        };
        let planner = matches!(agent, ChatAgentType::Planner)
            .then(|| SessionFlowContext::from_planner_history(messages));
        let sub_agent_scan = (!matches!(agent, ChatAgentType::Planner))
            .then(|| FlowHistoryScan::from_sub_agent_branch_messages(messages));
        Self {
            agent,
            flow_id,
            step_id,
            allowed_tools,
            planner,
            sub_agent_scan,
        }
    }

    pub fn explain_unavailable(&self, tool_name: &str) -> String {
        if !tools::is_known_queue_tool(tool_name) {
            return format!(
                "Unknown tool `{tool_name}`: not a registered platform tool. \
The model may have hallucinated a tool name or used a name from another product."
            );
        }

        if !tool_names_for_agent(&self.agent)
            .iter()
            .any(|n| *n == tool_name)
        {
            let owners = agents_that_expose_tool(tool_name);
            let owner_hint = if owners.is_empty() {
                "It is not exposed to any chat agent.".to_string()
            } else {
                format!("It is only available to: {}.", owners.join(", "))
            };
            return format!(
                "Tool `{tool_name}` exists on the platform but is not available to agent `{}`. {owner_hint}",
                agent_prompt_role_name(&self.agent),
            );
        }

        if self.allowed_tools.iter().any(|t| *t == tool_name) {
            return format!(
                "Tool `{tool_name}` is declared for this turn but could not be queued (internal error). \
Retry once; if it persists, the session may need a new message."
            );
        }

        let mut msg = format!("Tool `{tool_name}` is locked for this turn.");
        if let (Some(flow_id), Some(step_id)) = (self.flow_id, self.step_id) {
            msg.push_str(&format!(" Workflow: `{flow_id}` step `{step_id}`."));
        }
        msg.push_str(&format!(
            " Allowed tools this turn: {}.",
            if self.allowed_tools.is_empty() {
                "(none)".to_string()
            } else {
                self.allowed_tools.join(", ")
            }
        ));
        if let Some(detail) = self.specific_lock_reason(tool_name) {
            msg.push(' ');
            msg.push_str(&detail);
        }
        msg
    }

    fn specific_lock_reason(&self, tool_name: &str) -> Option<String> {
        match self.agent {
            ChatAgentType::Planner => self.planner.as_ref().and_then(|ctx| {
                planner_tool_lock_reason(ctx, self.step_id, tool_name)
            }),
            ChatAgentType::ObservabilityAnalyzer => self
                .sub_agent_scan
                .as_ref()
                .and_then(|scan| observability_tool_lock_reason(scan, self.step_id, tool_name)),
            ChatAgentType::ArchitectureAnalyzer
            | ChatAgentType::SolutionAnalyzer
            | ChatAgentType::Developer => self
                .sub_agent_scan
                .as_ref()
                .and_then(|scan| discover_units_tool_lock_reason(scan, tool_name)),
        }
    }

    pub fn developer_mutate_load_prerequisite_error(
        &self,
        tool_name: &str,
        args: &Value,
    ) -> Option<String> {
        if self.agent != ChatAgentType::Developer {
            return None;
        }
        let scan = self.sub_agent_scan.as_ref()?;
        if !matches!(tool_name, modify_unit::TOOL_NAME | remove_unit::TOOL_NAME) {
            return None;
        }
        if !scan.discover_units_prerequisites_met() {
            return None;
        }
        scan.mutate_prerequisite_for_unit_id_in_args(tool_name, args)
            .err()
    }
}

fn agents_that_expose_tool(tool_name: &str) -> Vec<&'static str> {
    let agents = [
        (ChatAgentType::Planner, tool_names_for_agent(&ChatAgentType::Planner)),
        (
            ChatAgentType::ArchitectureAnalyzer,
            tool_names_for_agent(&ChatAgentType::ArchitectureAnalyzer),
        ),
        (
            ChatAgentType::SolutionAnalyzer,
            tool_names_for_agent(&ChatAgentType::SolutionAnalyzer),
        ),
        (
            ChatAgentType::Developer,
            tool_names_for_agent(&ChatAgentType::Developer),
        ),
        (
            ChatAgentType::ObservabilityAnalyzer,
            tool_names_for_agent(&ChatAgentType::ObservabilityAnalyzer),
        ),
    ];
    agents
        .into_iter()
        .filter(|(_, names)| names.iter().any(|n| *n == tool_name))
        .map(|(a, _)| agent_prompt_role_name(&a))
        .collect()
}

fn planner_tool_lock_reason(
    ctx: &SessionFlowContext,
    step_id: Option<&str>,
    tool_name: &str,
) -> Option<String> {
    match tool_name {
        analyze_solution_tool::TOOL_NAME => {
            if ctx.can_access_solution() {
                return None;
            }
            let arch_done = ctx.ever_architecture();
            let arch_requested = ctx.ever_requested(analyze_architecture_tool::TOOL_NAME);
            Some(format!(
                "Prerequisite not met: `{arch}` must complete (a finished tool result in this session) before `{sol}` unlocks. \
Session state: architecture_analysis_completed={arch_done}, architecture_analysis_requested={arch_requested}. \
Current workflow step: `{step}` — only discovery tools are enabled until architecture completes.",
                arch = analyze_architecture_tool::TOOL_NAME,
                sol = analyze_solution_tool::TOOL_NAME,
                step = step_id.unwrap_or("discover")
            ))
        }
        implement_solution_tool::TOOL_NAME => {
            if ctx.can_access_implement() {
                return None;
            }
            let solution_done = ctx.ever_solution();
            let arch_done = ctx.ever_architecture();
            Some(format!(
                "Prerequisite not met: `{sol}` must complete (a finished tool result in this session) before `{imp}` unlocks. \
Session state: solution_analysis_completed={solution_done}, architecture_analysis_completed={arch_done}. \
If architecture is done but solution is not, stay in the plan-solution step and run `{sol}` first.",
                sol = analyze_solution_tool::TOOL_NAME,
                imp = implement_solution_tool::TOOL_NAME,
            ))
        }
        analyze_architecture_tool::TOOL_NAME
        | analyze_observability_tool::TOOL_NAME
        | load_latest_releases::TOOL_NAME => None,
        _ => Some(format!(
            "This tool is only enabled in the **implement** workflow step after both architecture and solution analyses have completed. \
Session state: architecture_analysis_completed={}, solution_analysis_completed={}.",
            ctx.ever_architecture(),
            ctx.ever_solution()
        )),
    }
}

fn discover_units_tool_lock_reason(scan: &FlowHistoryScan, tool_name: &str) -> Option<String> {
    const FINISH_AND_MUTATE: &[&str] = &[
        finish_architecture_analysis::TOOL_NAME,
        finish_solution_analysis::TOOL_NAME,
        finish_implementation::TOOL_NAME,
        create_unit::TOOL_NAME,
        modify_unit::TOOL_NAME,
        remove_unit::TOOL_NAME,
    ];
    if !FINISH_AND_MUTATE.contains(&tool_name) {
        return None;
    }
    if scan.discover_units_prerequisites_met() {
        return None;
    }
    Some(discover_units_prerequisites_detail(scan, tool_name))
}

fn discover_units_prerequisites_detail(scan: &FlowHistoryScan, tool_name: &str) -> String {
    let load = load_latest_production_release_units::TOOL_NAME;
    let list_ctx = list_unit_contexts::TOOL_NAME;
    let list_mod = list_modules_for_unit_contexts::TOOL_NAME;
    let mut parts = vec![discover_units_gate::lock_error_preamble(tool_name)];
    if !scan.ever_invoked(load) {
        parts.push(format!("Missing: call `{load}` at least once."));
    }
    if !scan.ever_invoked(list_ctx) {
        parts.push(format!("Missing: call `{list_ctx}` at least once."));
    }
    if scan.ever_invoked(load) && scan.ever_invoked(list_ctx) {
        match scan.last_list_unit_contexts_had_no_slugs() {
            Some(true) => parts.push(format!(
                "`{list_ctx}` returned an empty `context_slugs` list — `{list_mod}` is not required."
            )),
            Some(false) => parts.push(format!(
                "Missing: `{list_ctx}` returned a non-empty `context_slugs` list — call `{list_mod}` once (with those `context_slugs`) before `{tool_name}`."
            )),
            None => parts.push(format!(
                "Missing: a successful `{list_ctx}` result is required (or call `{list_ctx}` again if the last attempt failed)."
            )),
        }
    }
    parts.join(" ")
}

fn observability_tool_lock_reason(
    scan: &FlowHistoryScan,
    step_id: Option<&str>,
    tool_name: &str,
) -> Option<String> {
    let traces = fetch_release_traces::TOOL_NAME;
    let load = load_latest_production_release_units::TOOL_NAME;
    let finish = finish_observability_analysis::TOOL_NAME;

    match tool_name {
        t if t == load => {
            if scan.ever_invoked(traces) {
                return None;
            }
            Some(format!(
                "`{load}` unlocks only after `{traces}` has been invoked at least once in this run. \
Session state: {traces}_invoked=false. Current step: `{step}`.",
                step = step_id.unwrap_or("traces_first")
            ))
        }
        t if t == finish => {
            if scan.ever_invoked(traces) && scan.ever_invoked(load) {
                return None;
            }
            Some(format!(
                "`{finish}` unlocks only after both `{traces}` and `{load}` have been invoked in this run. \
Session state: {traces}_invoked={}, {load}_invoked={}.",
                scan.ever_invoked(traces),
                scan.ever_invoked(load),
            ))
        }
        t if t == traces => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::chat_messages::RequestedToolCall;
    use crate::services::chat_worker::agent_flow::resolve_for_agent;
    use crate::services::chat_worker::llm::tools::list_unit_contexts::ToolListUnitContextsPayload;

    fn ai_tools(names: &[&str]) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        let payload = crate::models::chat_messages::TextPayload {
            text: String::new(),
            requested_tool_calls: Some(
                names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| RequestedToolCall {
                        tool_call_id: Some(i.to_string()),
                        tool_name: (*name).to_string(),
                        tool_arguments_to_pass: serde_json::json!({}),
                        thought_signature: None,
                    })
                    .collect(),
            ),
            thinking_blocks: None,
        };
        (
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(serde_json::to_value(&payload).unwrap()),
        )
    }

    #[test]
    fn unknown_tool_message() {
        let flow = AgentFlowSession::none();
        let gate = ToolGateDiagnostics::build(ChatAgentType::Planner, &[], &flow);
        let msg = gate.explain_unavailable("not_a_real_tool");
        assert!(msg.contains("Unknown tool"));
        assert!(msg.contains("not_a_real_tool"));
    }

    #[test]
    fn wrong_agent_tool_message() {
        let flow = resolve_for_agent(&ChatAgentType::Planner, &[]);
        let gate = ToolGateDiagnostics::build(ChatAgentType::Planner, &[], &flow);
        let msg = gate.explain_unavailable(create_unit::TOOL_NAME);
        assert!(msg.contains("not available to agent `Orchestrator`"));
        assert!(msg.contains("only available to: Programmer"));
    }

    #[test]
    fn planner_locked_solution_in_discover() {
        let flow = resolve_for_agent(&ChatAgentType::Planner, &[]);
        let gate = ToolGateDiagnostics::build(ChatAgentType::Planner, &[], &flow);
        let msg = gate.explain_unavailable(analyze_solution_tool::TOOL_NAME);
        assert!(msg.contains("locked"));
        assert!(msg.contains(analyze_architecture_tool::TOOL_NAME));
        assert!(msg.contains(analyze_solution_tool::TOOL_NAME));
        assert!(msg.contains("architecture_analysis_completed=false"));
    }

    #[test]
    fn developer_finish_locked_before_discover() {
        let flow = resolve_for_agent(&ChatAgentType::Developer, &[]);
        let gate = ToolGateDiagnostics::build(ChatAgentType::Developer, &[], &flow);
        let msg = gate.explain_unavailable(finish_implementation::TOOL_NAME);
        assert!(msg.contains("unit-discovery prerequisites"));
        assert!(msg.contains("context_slugs"));
        assert!(msg.contains(load_latest_production_release_units::TOOL_NAME));
        assert!(msg.contains(list_unit_contexts::TOOL_NAME));
        assert!(msg.contains("Missing: call"));
    }

    #[test]
    fn observability_finish_locked_without_traces() {
        let flow = resolve_for_agent(&ChatAgentType::ObservabilityAnalyzer, &[]);
        let gate = ToolGateDiagnostics::build(ChatAgentType::ObservabilityAnalyzer, &[], &flow);
        let msg = gate.explain_unavailable(finish_observability_analysis::TOOL_NAME);
        assert!(msg.contains("fetch_release_traces"));
        assert!(msg.contains(&format!("{}_invoked=false", fetch_release_traces::TOOL_NAME)));
    }

    #[test]
    fn discover_modules_required_when_contexts_nonempty() {
        let p = ToolListUnitContextsPayload {
            args: Default::default(),
            context_slugs: vec!["ctx-a".into()],
            tool_call_id: None,
            tool_name: Some(list_unit_contexts::TOOL_NAME.to_string()),
            error: None,
        };
        let messages = vec![
            ai_tools(&[load_latest_production_release_units::TOOL_NAME]),
            (
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolFetchRelevantUnits,
                None,
            ),
            ai_tools(&[list_unit_contexts::TOOL_NAME]),
            (
                ChatSessionRoleType::Ai,
                ChatMessageType::ToolListUnitContexts,
                Some(serde_json::to_value(&p).unwrap()),
            ),
        ];
        let flow = resolve_for_agent(&ChatAgentType::Developer, &messages);
        let gate = ToolGateDiagnostics::build(ChatAgentType::Developer, &messages, &flow);
        let msg = gate.explain_unavailable(create_unit::TOOL_NAME);
        assert!(msg.contains(list_modules_for_unit_contexts::TOOL_NAME));
    }
}
