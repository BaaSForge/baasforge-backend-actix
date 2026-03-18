mod architecture_analyzer;
mod developer;
mod planner;
mod solution_analyzer;

use crate::database::PooledConn;
use crate::models::chat_messages::{ChatAgentType, ChatMessageType, ChatSessionRoleType};
use diesel::result::Error;
use serde_json::Value;
use uuid::Uuid;

pub fn tool_names_for_agent(agent: &ChatAgentType) -> &'static [&'static str] {
    match agent {
        ChatAgentType::Planner => planner::tool_names(),
        ChatAgentType::ArchitectureAnalyzer => architecture_analyzer::tool_names(),
        ChatAgentType::SolutionAnalyzer => solution_analyzer::tool_names(),
        ChatAgentType::Developer => developer::tool_names(),
    }
}

pub fn system_prompt_for_agent(agent: &ChatAgentType) -> &'static str {
    match agent {
        ChatAgentType::Planner => planner::SYSTEM_PROMPT,
        ChatAgentType::ArchitectureAnalyzer => architecture_analyzer::SYSTEM_PROMPT,
        ChatAgentType::SolutionAnalyzer => solution_analyzer::SYSTEM_PROMPT,
        ChatAgentType::Developer => developer::SYSTEM_PROMPT,
    }
}

pub fn load_history_for_agent(
    conn: &mut PooledConn,
    session_id: Uuid,
    agent: ChatAgentType,
) -> Result<Vec<(ChatSessionRoleType, ChatMessageType, Option<Value>)>, Error> {
    match agent {
        ChatAgentType::Planner => planner::load_history(conn, session_id),
        ChatAgentType::ArchitectureAnalyzer => {
            architecture_analyzer::load_history(conn, session_id)
        }
        ChatAgentType::SolutionAnalyzer => solution_analyzer::load_history(conn, session_id),
        ChatAgentType::Developer => developer::load_history(conn, session_id),
    }
}
