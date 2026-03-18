mod architecture_analyzer;
mod developer;
mod planner;
mod solution_analyzer;

use crate::database::PooledConn;
use crate::models::chat_messages::{ChatAgentType, ChatSessionRoleType};
use diesel::result::Error;
use serde_json::Value;
use uuid::Uuid;

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
) -> Result<Vec<(ChatSessionRoleType, Option<Value>)>, Error> {
    match agent {
        ChatAgentType::Planner => planner::load_history(conn, session_id),
        ChatAgentType::ArchitectureAnalyzer => architecture_analyzer::load_history(conn, session_id),
        ChatAgentType::SolutionAnalyzer => solution_analyzer::load_history(conn, session_id),
        ChatAgentType::Developer => developer::load_history(conn, session_id),
    }
}
