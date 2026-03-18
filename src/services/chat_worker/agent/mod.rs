mod architecture_analyzer;
mod developer;
mod planner;
mod solution_analyzer;

use crate::models::chat_messages::ChatAgentType;

pub fn system_prompt_for_agent(agent: &ChatAgentType) -> &'static str {
    match agent {
        ChatAgentType::Planner => planner::SYSTEM_PROMPT,
        ChatAgentType::ArchitectureAnalyzer => architecture_analyzer::SYSTEM_PROMPT,
        ChatAgentType::SolutionAnalyzer => solution_analyzer::SYSTEM_PROMPT,
        ChatAgentType::Developer => developer::SYSTEM_PROMPT,
    }
}
