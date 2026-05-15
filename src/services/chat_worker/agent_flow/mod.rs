mod context;
mod engine;
pub mod flows;
mod history_scan;

use crate::models::chat_messages::{ChatAgentType, ChatMessageType, ChatSessionRoleType};
use flows::architecture_analyzer_flow::ARCHITECTURE_ANALYZER_FLOW;
use flows::developer_flow::DEVELOPER_FLOW;
use flows::observability_analyzer_flow::OBSERVABILITY_ANALYZER_FLOW;
use flows::planner_flow::PLANNER_FLOW;
use flows::solution_analyzer_flow::SOLUTION_ANALYZER_FLOW;
use serde_json::Value;

pub use context::SessionFlowContext;
pub use engine::{FlowDefinition, FlowResolution, FlowStep, resolve_flow};
pub use history_scan::{
    tool_name_for_ai_tool_result_type, FlowHistoryScan, SubAgentBranchContext, DISCOVER_UNITS_TRIO,
};

#[derive(Debug, Clone)]
pub struct AgentFlowSession {
    pub resolution: Option<FlowResolution>,
}

impl AgentFlowSession {
    pub fn none() -> Self {
        Self { resolution: None }
    }

    pub fn tool_names<'a>(
        &'a self,
        _agent: &ChatAgentType,
        default: &'a [&'static str],
    ) -> Vec<&'static str> {
        match &self.resolution {
            Some(r) => r.allowed_tool_names.clone(),
            None => default.to_vec(),
        }
    }

    pub fn prompt_appendix(&self) -> Option<&str> {
        self.resolution
            .as_ref()
            .and_then(|r| r.prompt_appendix.as_deref())
    }
}

pub fn resolve_for_agent(
    agent: &ChatAgentType,
    messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
) -> AgentFlowSession {
    match agent {
        ChatAgentType::Planner => {
            let ctx = SessionFlowContext::from_planner_history(messages);
            let resolution = resolve_flow(&PLANNER_FLOW, &ctx);
            AgentFlowSession {
                resolution: Some(resolution),
            }
        }
        ChatAgentType::ArchitectureAnalyzer => {
            let ctx = SubAgentBranchContext::from_messages(messages);
            let resolution = resolve_flow(&ARCHITECTURE_ANALYZER_FLOW, &ctx);
            AgentFlowSession {
                resolution: Some(resolution),
            }
        }
        ChatAgentType::SolutionAnalyzer => {
            let ctx = SubAgentBranchContext::from_messages(messages);
            let resolution = resolve_flow(&SOLUTION_ANALYZER_FLOW, &ctx);
            AgentFlowSession {
                resolution: Some(resolution),
            }
        }
        ChatAgentType::ObservabilityAnalyzer => {
            let ctx = SubAgentBranchContext::from_messages(messages);
            let resolution = resolve_flow(&OBSERVABILITY_ANALYZER_FLOW, &ctx);
            AgentFlowSession {
                resolution: Some(resolution),
            }
        }
        ChatAgentType::Developer => {
            let ctx = SubAgentBranchContext::from_messages(messages);
            let resolution = resolve_flow(&DEVELOPER_FLOW, &ctx);
            AgentFlowSession {
                resolution: Some(resolution),
            }
        }
    }
}
