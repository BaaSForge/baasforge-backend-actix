//! When production’s live release no longer matches the snapshot on the chat session
//! (`live_production_release_id` at creation), we append a strict warning: a release staged or
//! promoted **from this chat** is built from this session’s baseline—not today’s production—so it
//! **will** supersede current production and thereby **revert** any production release that came from
//! other chat sessions. LLMs cannot ship; they only explain. Only the **Planner** receives this
//! appendix—the **Developer** is not told about stale baselines. Unit tools stay enabled (units are
//! per-session).

use crate::models::chat_messages::ChatAgentType;
use std::borrow::Cow;

use super::{
    architecture_analyzer, developer, planner, solution_analyzer, tool_names_for_agent,
};

const PLANNER_STALE_BASE_APPENDIX: &str = r#"

## Stale production baseline (system — obey)
This chat’s **saved baseline** is **not** the production release that is **live** now: production was advanced from **another** chat session (or otherwise moved on). **Units stay scoped to this chat**—planning and unit work here is fine.

**Releases — you cannot ship:** You do **not** create stage or production releases; only the user does in the UI. Your job is to **warn** so they understand consequences.

**What actually happens:** A stage or production release **from this chat** is computed from this session’s old baseline. It **will** replace whatever is live on production when promoted—so it **will revert** the production deployment that came from **other** chat sessions, because that newer release is **not** this chat’s base. State this plainly; do not soften with “may”.

**If they still want to ship:** The user may **intentionally** stage or promote here to **explicitly revert** those other releases (stage and/or production)—that is allowed; make sure they understand they are choosing to overwrite current production with this chat’s line of work.

**Usually safer:** A **new chat** created while aligned with **current** production avoids this revert unless they mean to do it."#;

pub fn system_prompt_for_agent(agent: &ChatAgentType, stale_base_release: bool) -> Cow<'static, str> {
    let base: &'static str = match agent {
        ChatAgentType::Planner => *planner::SYSTEM_PROMPT_WITH_UNIT_BRIEFS,
        ChatAgentType::ArchitectureAnalyzer => *architecture_analyzer::SYSTEM_PROMPT_WITH_UNIT_DETAILS,
        ChatAgentType::SolutionAnalyzer => *solution_analyzer::SYSTEM_PROMPT_WITH_UNIT_DETAILS,
        ChatAgentType::Developer => *developer::SYSTEM_PROMPT_WITH_UNIT_DETAILS,
    };
    if !stale_base_release {
        return Cow::Borrowed(base);
    }
    match agent {
        ChatAgentType::Planner => Cow::Owned(format!("{base}{PLANNER_STALE_BASE_APPENDIX}")),
        _ => Cow::Borrowed(base),
    }
}

/// Tool names exposed to the LLM and queue callbacks for this agent (full set; stale baseline does not disable tools).
pub fn tool_names_for_llm_session(agent: &ChatAgentType) -> Vec<&'static str> {
    tool_names_for_agent(agent).to_vec()
}
