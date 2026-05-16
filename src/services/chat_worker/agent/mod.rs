mod architecture_analyzer;
mod developer;
mod nested_branch_history;
mod observability_analyzer;
mod planner;
mod session_stale_base_release;
mod solution_analyzer;

pub use developer::tool_message_types as developer_tool_message_types;
pub use nested_branch_history::{
    load_nested_subtree_descendant_rows, subtree_message_ids, SessionHistoryLoad,
};
pub use session_stale_base_release::{system_prompt_for_agent, tool_names_for_llm_session};

use crate::database::PooledConn;
use crate::models::chat_messages::ChatAgentType;
use crate::services::chat_worker::llm::history::{
    filter_session_messages_for_upstream_llm,
    remove_tool_result_messages_without_matching_assistant_tool_use,
    trim_assistant_requested_tool_calls_to_match_following_tool_results,
};
use diesel::result::Error;
use uuid::Uuid;

/// Display name for this agent in tool errors and cross-agent references (Orchestrator, Architecture
/// Analyzer, Solution Analyzer, Programmer, Observability Analyzer).
pub fn agent_prompt_role_name(agent: &ChatAgentType) -> &'static str {
    match agent {
        ChatAgentType::Planner => "Orchestrator",
        ChatAgentType::ArchitectureAnalyzer => "Architecture Analyzer",
        ChatAgentType::SolutionAnalyzer => "Solution Analyzer",
        ChatAgentType::Developer => "Programmer",
        ChatAgentType::ObservabilityAnalyzer => "Observability Analyzer",
    }
}

pub fn tool_names_for_agent(agent: &ChatAgentType) -> &'static [&'static str] {
    match agent {
        ChatAgentType::Planner => planner::tool_names(),
        ChatAgentType::ArchitectureAnalyzer => architecture_analyzer::tool_names(),
        ChatAgentType::SolutionAnalyzer => solution_analyzer::tool_names(),
        ChatAgentType::Developer => developer::tool_names(),
        ChatAgentType::ObservabilityAnalyzer => observability_analyzer::tool_names(),
    }
}

/// Fourth argument meaning depends on agent. For [`ChatAgentType::ArchitectureAnalyzer`],
/// [`ChatAgentType::SolutionAnalyzer`], [`ChatAgentType::ObservabilityAnalyzer`], and [`ChatAgentType::Developer`] it **must** be the DB id of the
/// corresponding tool row (`ToolAnalyzeArchitecture` / `ToolAnalyzeSolution` / `ToolAnalyzeObservability` / `ToolImplementSolution`,
/// same as queue `invoking_message_id` for that job); history is only that nested subtree. For other agents,
/// see each `load_history` (Planner: unused; etc.).
///
/// **Planner prompt alignment:** Sub-agents do **not** share history with each other. Each tool invocation
/// starts a **new** nested thread for that agent; they have no access to other agents’ past runs or to
/// their own prior invocations. The only context the planner reliably injects is the **`intent`** or
/// **`directive`** on the tool call (plus whatever that agent loads via its own tools). Anything the next
/// sub-agent must “know” from a previous step must be repeated or summarized there by the planner.
pub fn load_history_for_agent(
    conn: &mut PooledConn,
    session_id: Uuid,
    agent: ChatAgentType,
    thread_anchor_message_id: Option<Uuid>,
) -> Result<SessionHistoryLoad, Error> {
    let SessionHistoryLoad {
        mut session_messages,
        db_rows_chronological,
    } = match agent {
        ChatAgentType::Planner => planner::load_history(conn, session_id, thread_anchor_message_id),
        ChatAgentType::ArchitectureAnalyzer => {
            architecture_analyzer::load_history(conn, session_id, thread_anchor_message_id)
        }
        ChatAgentType::SolutionAnalyzer => {
            solution_analyzer::load_history(conn, session_id, thread_anchor_message_id)
        }
        ChatAgentType::Developer => {
            developer::load_history(conn, session_id, thread_anchor_message_id)
        }
        ChatAgentType::ObservabilityAnalyzer => {
            observability_analyzer::load_history(conn, session_id, thread_anchor_message_id)
        }
    }?;
    session_messages = filter_session_messages_for_upstream_llm(session_messages);
    remove_tool_result_messages_without_matching_assistant_tool_use(&mut session_messages);
    trim_assistant_requested_tool_calls_to_match_following_tool_results(&mut session_messages);
    Ok(SessionHistoryLoad {
        session_messages,
        db_rows_chronological,
    })
}
