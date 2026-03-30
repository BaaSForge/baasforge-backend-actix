//! Read-only session policy for LLM: which system prompt appendices apply and which tool names are
//! stripped. Kept in one place next to [`super::tool_names_for_agent`].
//!
//! Tool filters cannot import `crate::services::chat_worker::llm::tools` — that would create a
//! module cycle (`agent` → `llm::tools` → `agent`). The `READ_ONLY_EXCLUDED_*` constants must match
//! each tool module’s `TOOL_NAME`.

use crate::models::chat_messages::ChatAgentType;
use std::borrow::Cow;

use super::{
    architecture_analyzer, developer, planner, solution_analyzer, tool_names_for_agent,
};

const PLANNER_READ_ONLY_APPENDIX: &str = r#"

## Read-only session (system — obey)
Production’s live release on this project has moved to a revision that **this chat session is not allowed to advance** (it no longer matches the baseline captured when the session started, and the current live production release was created from another conversation). **Do not** tell the user you can implement, edit, or delete units. The **implement solution** tool is **not** available in this session.

You **may** still use **analyze architecture**, **analyze solution**, and **load_latest_releases** to answer questions in a read-only way. Explain clearly that this chat is read-only for shipping and unit changes: the user should open a **new chat** (or work in a session tied to the current production line) to make releaseable changes. Stage and production release actions are also blocked in the API for the same reason."#;

const DEVELOPER_READ_ONLY_APPENDIX: &str = r#"

## Read-only session (system — obey)
This run cannot change units: production’s live release no longer matches this session’s release lineage. **Do not** call create_unit, modify_unit, or remove_unit — those tools are disabled. Call **finish implementation** once with a **detailed_report** that states no code or unit changes were made and that the session is read-only until the user works from an aligned chat."#;

/// Must match [`crate::services::chat_worker::llm::tools::implement_solution_tool::TOOL_NAME`].
const READ_ONLY_EXCLUDE_PLANNER: &str = "implement_solution_tool";
/// Must match [`crate::services::chat_worker::llm::tools::create_unit::TOOL_NAME`].
const READ_ONLY_EXCLUDE_DEV_CREATE: &str = "create_unit";
/// Must match [`crate::services::chat_worker::llm::tools::modify_unit::TOOL_NAME`].
const READ_ONLY_EXCLUDE_DEV_MODIFY: &str = "modify_unit";
/// Must match [`crate::services::chat_worker::llm::tools::remove_unit::TOOL_NAME`].
const READ_ONLY_EXCLUDE_DEV_REMOVE: &str = "remove_unit";

pub fn system_prompt_for_agent(agent: &ChatAgentType, session_read_only: bool) -> Cow<'static, str> {
    let base: &'static str = match agent {
        ChatAgentType::Planner => *planner::SYSTEM_PROMPT_WITH_UNIT_BRIEFS,
        ChatAgentType::ArchitectureAnalyzer => *architecture_analyzer::SYSTEM_PROMPT_WITH_UNIT_DETAILS,
        ChatAgentType::SolutionAnalyzer => *solution_analyzer::SYSTEM_PROMPT_WITH_UNIT_DETAILS,
        ChatAgentType::Developer => *developer::SYSTEM_PROMPT_WITH_UNIT_DETAILS,
    };
    if !session_read_only {
        return Cow::Borrowed(base);
    }
    match agent {
        ChatAgentType::Planner => Cow::Owned(format!("{base}{PLANNER_READ_ONLY_APPENDIX}")),
        ChatAgentType::Developer => Cow::Owned(format!("{base}{DEVELOPER_READ_ONLY_APPENDIX}")),
        _ => Cow::Borrowed(base),
    }
}

/// Tool names exposed to the LLM and queue callbacks for this agent, honoring read-only sessions.
pub fn tool_names_for_llm_session(
    agent: &ChatAgentType,
    session_read_only: bool,
) -> Vec<&'static str> {
    let base = tool_names_for_agent(agent);
    if !session_read_only {
        return base.to_vec();
    }
    match agent {
        ChatAgentType::Planner => base
            .iter()
            .copied()
            .filter(|&n| n != READ_ONLY_EXCLUDE_PLANNER)
            .collect(),
        ChatAgentType::Developer => base
            .iter()
            .copied()
            .filter(|&n| {
                !matches!(
                    n,
                    READ_ONLY_EXCLUDE_DEV_CREATE
                        | READ_ONLY_EXCLUDE_DEV_MODIFY
                        | READ_ONLY_EXCLUDE_DEV_REMOVE
                )
            })
            .collect(),
        _ => base.to_vec(),
    }
}
