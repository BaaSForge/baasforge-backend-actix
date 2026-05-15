use crate::models::chat_messages::{ChatMessageType, ChatSessionRoleType, TextPayload};
use crate::services::chat_worker::agent_flow::history_scan::{
    tool_name_for_ai_tool_result_type, tool_names_from_text_payload, FlowHistoryScan,
};
use crate::services::chat_worker::llm::tools::{
    analyze_architecture_tool, analyze_observability_tool, analyze_solution_tool,
    implement_solution_tool, load_latest_releases,
};
use serde_json::Value;

const ARCHITECTURE: &str = analyze_architecture_tool::TOOL_NAME;
const SOLUTION: &str = analyze_solution_tool::TOOL_NAME;
const IMPLEMENT: &str = implement_solution_tool::TOOL_NAME;
const OBSERVABILITY: &str = analyze_observability_tool::TOOL_NAME;
const LOAD_RELEASES: &str = load_latest_releases::TOOL_NAME;

const ORCHESTRATION_TOOLS: &[&str] = &[ARCHITECTURE, SOLUTION, IMPLEMENT];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowEventKind {
    User,
    PlannerText,
    PlannerToolRequested,
    PlannerToolResult,
}

#[derive(Debug, Clone)]
pub struct FlowEvent {
    pub index: usize,
    pub kind: FlowEventKind,
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionFlowContext {
    pub message_count: usize,
    pub events: Vec<FlowEvent>,
    /// Full tool request/result index for this slice (same ingest as sub-agents). Planner-specific
    /// helpers (`ever_architecture`, sequences, …) use `events` and slotted counts; use `tools`
    /// when you need sub-agent-style `ever_invoked` on the planner thread.
    pub tools: FlowHistoryScan,
    planner_tool_requested: [usize; 5],
    planner_tool_result: [usize; 5],
}

impl SessionFlowContext {
    pub fn from_planner_history(
        messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    ) -> Self {
        let mut ctx = Self {
            message_count: messages.len(),
            events: Vec::with_capacity(messages.len()),
            tools: FlowHistoryScan::default(),
            planner_tool_requested: [0; 5],
            planner_tool_result: [0; 5],
        };

        for (index, (role, msg_type, payload)) in messages.iter().enumerate() {
            ctx.tools
                .ingest_ai_message(role, msg_type, payload);
            match (role, msg_type) {
                (ChatSessionRoleType::User, _) => {
                    ctx.push_event(index, FlowEventKind::User, None);
                }
                (
                    ChatSessionRoleType::Ai,
                    ChatMessageType::Text,
                ) if is_planner_ai(role, payload) => {
                    let names = tool_names_from_text_payload(payload);
                    if names.is_empty() {
                        ctx.push_event(index, FlowEventKind::PlannerText, None);
                    } else {
                        for name in &names {
                            ctx.record_request(name);
                            ctx.push_event(
                                index,
                                FlowEventKind::PlannerToolRequested,
                                Some(name.clone()),
                            );
                        }
                    }
                }
                (ChatSessionRoleType::Ai, tool_type) => {
                    if let Some(name) = tool_name_for_ai_tool_result_type(tool_type) {
                        ctx.record_result(name);
                        if tool_slot(name).is_some() {
                            ctx.push_event(
                                index,
                                FlowEventKind::PlannerToolResult,
                                Some(name.to_string()),
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        ctx
    }

    fn push_event(&mut self, index: usize, kind: FlowEventKind, tool_name: Option<String>) {
        self.events.push(FlowEvent {
            index,
            kind,
            tool_name,
        });
    }

    fn record_request(&mut self, name: &str) {
        if let Some(i) = tool_slot(name) {
            self.planner_tool_requested[i] += 1;
        }
    }

    fn record_result(&mut self, name: &str) {
        if let Some(i) = tool_slot(name) {
            self.planner_tool_result[i] += 1;
        }
    }

    pub fn planner_ai_turn_count(&self) -> usize {
        self.events
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    FlowEventKind::PlannerText | FlowEventKind::PlannerToolRequested
                )
            })
            .map(|e| e.index)
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    pub fn user_message_count(&self) -> usize {
        self.events
            .iter()
            .filter(|e| e.kind == FlowEventKind::User)
            .count()
    }

    pub fn ever_requested(&self, tool: &str) -> bool {
        tool_slot(tool).is_some_and(|i| self.planner_tool_requested[i] > 0)
    }

    pub fn ever_has_result(&self, tool: &str) -> bool {
        tool_slot(tool).is_some_and(|i| self.planner_tool_result[i] > 0)
    }

    pub fn request_count(&self, tool: &str) -> usize {
        tool_slot(tool)
            .map(|i| self.planner_tool_requested[i])
            .unwrap_or(0)
    }

    pub fn result_count(&self, tool: &str) -> usize {
        tool_slot(tool)
            .map(|i| self.planner_tool_result[i])
            .unwrap_or(0)
    }

    pub fn ever_architecture(&self) -> bool {
        self.ever_touched(ARCHITECTURE)
    }

    pub fn ever_solution(&self) -> bool {
        self.ever_touched(SOLUTION)
    }

    pub fn ever_implement(&self) -> bool {
        self.ever_touched(IMPLEMENT)
    }

    pub fn ever_observability(&self) -> bool {
        self.ever_touched(OBSERVABILITY)
    }

    pub fn ever_orchestration_tool(&self) -> bool {
        ORCHESTRATION_TOOLS.iter().any(|t| self.ever_touched(t))
    }

    fn ever_touched(&self, tool: &str) -> bool {
        self.ever_has_result(tool)
    }

    pub fn messages_since_last(&self, tool: &str) -> Option<usize> {
        let last_idx = self
            .events
            .iter()
            .rev()
            .find(|e| e.tool_name.as_deref() == Some(tool))
            .map(|e| e.index)?;
        Some(self.message_count.saturating_sub(1).saturating_sub(last_idx))
    }

    pub fn messages_since_first(&self, tool: &str) -> Option<usize> {
        let first_idx = self
            .events
            .iter()
            .find(|e| e.tool_name.as_deref() == Some(tool))
            .map(|e| e.index)?;
        Some(self.message_count.saturating_sub(1).saturating_sub(first_idx))
    }

    pub fn has_ordered_sequence(&self, tools: &[&str], within_last_messages: Option<usize>) -> bool {
        let start_index = within_last_messages.map(|n| {
            self.message_count.saturating_sub(n)
        });
        let mut need = 0usize;
        for event in &self.events {
            if let Some(min) = start_index {
                if event.index < min {
                    continue;
                }
            }
            let Some(name) = event.tool_name.as_deref() else {
                continue;
            };
            if name == tools[need] {
                need += 1;
                if need == tools.len() {
                    return true;
                }
            }
        }
        false
    }

    pub fn has_full_develop_orchestration_in_window(&self, window: usize) -> bool {
        self.has_ordered_sequence(&[ARCHITECTURE, SOLUTION, IMPLEMENT], Some(window))
    }

    pub fn has_solution_without_following_implement(&self) -> bool {
        let mut last_solution: Option<usize> = None;
        let mut implement_after_last_solution = false;
        for event in &self.events {
            match event.tool_name.as_deref() {
                Some(SOLUTION) => {
                    last_solution = Some(event.index);
                    implement_after_last_solution = false;
                }
                Some(IMPLEMENT) if last_solution.is_some() => {
                    if event.index > last_solution.unwrap() {
                        implement_after_last_solution = true;
                    }
                }
                _ => {}
            }
        }
        last_solution.is_some() && !implement_after_last_solution
    }

    pub fn last_develop_orchestration_tool(&self) -> Option<&str> {
        self.events
            .iter()
            .rev()
            .filter_map(|e| e.tool_name.as_deref())
            .find(|n| ORCHESTRATION_TOOLS.contains(n))
    }

    pub fn is_pre_architecture_phase(&self) -> bool {
        !self.ever_architecture() && !self.ever_solution() && !self.ever_implement()
    }

    pub fn can_access_solution(&self) -> bool {
        self.ever_architecture()
    }

    pub fn can_access_implement(&self) -> bool {
        self.ever_solution()
    }

    pub fn has_architecture_without_following_solution(&self) -> bool {
        let mut last_arch: Option<usize> = None;
        let mut solution_after_last_arch = false;
        for event in &self.events {
            match event.tool_name.as_deref() {
                Some(ARCHITECTURE) => {
                    last_arch = Some(event.index);
                    solution_after_last_arch = false;
                }
                Some(SOLUTION) if last_arch.is_some() && event.index > last_arch.unwrap() => {
                    solution_after_last_arch = true;
                }
                _ => {}
            }
        }
        last_arch.is_some() && !solution_after_last_arch
    }

    pub fn develop_orchestration_touch_count(&self) -> usize {
        ORCHESTRATION_TOOLS
            .iter()
            .map(|t| self.request_count(t) + self.result_count(t))
            .sum()
    }

    pub fn messages_since_last_user(&self) -> Option<usize> {
        let last_user = self
            .events
            .iter()
            .rev()
            .find(|e| e.kind == FlowEventKind::User)
            .map(|e| e.index)?;
        Some(self.message_count.saturating_sub(1).saturating_sub(last_user))
    }

    pub fn ever_touched_since_last_user(&self, tool: &str) -> bool {
        let Some(since) = self.messages_since_last_user() else {
            return self.ever_touched(tool);
        };
        let min_index = self.message_count.saturating_sub(since);
        self.events.iter().any(|e| {
            e.index >= min_index && e.tool_name.as_deref() == Some(tool)
        })
    }

    pub fn is_observability_only_orchestration(&self) -> bool {
        self.ever_observability()
            && !self.ever_architecture()
            && !self.ever_solution()
            && !self.ever_implement()
    }
}

fn is_planner_ai(role: &ChatSessionRoleType, payload: &Option<Value>) -> bool {
    matches!(role, ChatSessionRoleType::Ai)
        && payload.is_some()
}

fn tool_slot(name: &str) -> Option<usize> {
    match name {
        ARCHITECTURE => Some(0),
        SOLUTION => Some(1),
        IMPLEMENT => Some(2),
        OBSERVABILITY => Some(3),
        LOAD_RELEASES => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::chat_messages::RequestedToolCall;

    fn user_msg(text: &str) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        (
            ChatSessionRoleType::User,
            ChatMessageType::Text,
            Some(serde_json::json!({ "text": text })),
        )
    }

    fn planner_text_only(text: &str) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        let payload = TextPayload {
            text: text.into(),
            requested_tool_calls: None,
            thinking_blocks: None,
        };
        (
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(serde_json::to_value(&payload).unwrap()),
        )
    }

    fn planner_tool_call(name: &str) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        planner_tool_calls(&[name])
    }

    fn planner_tool_calls(names: &[&str]) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        let payload = TextPayload {
            text: String::new(),
            requested_tool_calls: Some(
                names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| RequestedToolCall {
                        tool_call_id: Some(i.to_string()),
                        tool_name: (*name).into(),
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

    fn tool_result(msg_type: ChatMessageType) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        (ChatSessionRoleType::Ai, msg_type, None)
    }

    #[test]
    fn empty_history_is_pre_architecture() {
        let ctx = SessionFlowContext::from_planner_history(&[]);
        assert!(ctx.is_pre_architecture_phase());
        assert!(!ctx.ever_orchestration_tool());
        assert!(!ctx.can_access_solution());
        assert!(!ctx.can_access_implement());
        assert_eq!(ctx.message_count, 0);
        assert_eq!(ctx.user_message_count(), 0);
    }

    #[test]
    fn observability_only_session() {
        let messages = vec![
            user_msg("debug prod"),
            planner_tool_call(OBSERVABILITY),
            tool_result(ChatMessageType::ToolAnalyzeObservability),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(ctx.is_observability_only_orchestration());
        assert!(ctx.is_pre_architecture_phase());
        assert!(!ctx.can_access_solution());
        assert!(ctx.ever_observability());
        assert_eq!(ctx.request_count(OBSERVABILITY), 1);
        assert_eq!(ctx.result_count(OBSERVABILITY), 1);
    }

    #[test]
    fn architecture_request_without_result_does_not_unlock_solution() {
        let messages = vec![user_msg("plan"), planner_tool_call(ARCHITECTURE)];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(ctx.ever_requested(ARCHITECTURE));
        assert!(!ctx.ever_has_result(ARCHITECTURE));
        assert!(!ctx.ever_architecture());
        assert!(!ctx.can_access_solution());
        assert!(ctx.is_pre_architecture_phase());
    }

    #[test]
    fn architecture_result_unlocks_solution() {
        let messages = vec![
            planner_tool_call(ARCHITECTURE),
            tool_result(ChatMessageType::ToolAnalyzeArchitecture),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(ctx.ever_architecture());
        assert!(ctx.can_access_solution());
        assert!(!ctx.can_access_implement());
        assert!(ctx.has_architecture_without_following_solution());
    }

    #[test]
    fn solution_result_unlocks_implement() {
        let messages = vec![
            planner_tool_call(ARCHITECTURE),
            tool_result(ChatMessageType::ToolAnalyzeArchitecture),
            planner_tool_call(SOLUTION),
            tool_result(ChatMessageType::ToolAnalyzeSolution),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(ctx.can_access_implement());
        assert!(!ctx.has_architecture_without_following_solution());
        assert!(ctx.has_solution_without_following_implement());
    }

    #[test]
    fn implement_after_solution_clears_pending_flag() {
        let messages = vec![
            planner_tool_call(SOLUTION),
            tool_result(ChatMessageType::ToolAnalyzeSolution),
            planner_tool_call(IMPLEMENT),
            tool_result(ChatMessageType::ToolImplementSolution),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(!ctx.has_solution_without_following_implement());
        assert_eq!(ctx.last_develop_orchestration_tool(), Some(IMPLEMENT));
    }

    #[test]
    fn ordered_sequence_detected() {
        let messages = vec![
            user_msg("hi"),
            planner_tool_call(ARCHITECTURE),
            tool_result(ChatMessageType::ToolAnalyzeArchitecture),
            planner_tool_call(SOLUTION),
            tool_result(ChatMessageType::ToolAnalyzeSolution),
            planner_tool_call(IMPLEMENT),
            tool_result(ChatMessageType::ToolImplementSolution),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(ctx.has_full_develop_orchestration_in_window(50));
        assert!(ctx.has_ordered_sequence(&[ARCHITECTURE, SOLUTION, IMPLEMENT], None));
        assert!(ctx.can_access_implement());
        assert_eq!(ctx.develop_orchestration_touch_count(), 6);
    }

    #[test]
    fn ordered_sequence_outside_window_not_detected() {
        let mut messages = vec![
            user_msg("start"),
            planner_tool_call(ARCHITECTURE),
            tool_result(ChatMessageType::ToolAnalyzeArchitecture),
            planner_tool_call(SOLUTION),
            tool_result(ChatMessageType::ToolAnalyzeSolution),
            planner_tool_call(IMPLEMENT),
            tool_result(ChatMessageType::ToolImplementSolution),
        ];
        for _ in 0..15 {
            messages.push(planner_text_only("filler"));
        }
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(ctx.has_full_develop_orchestration_in_window(50));
        assert!(!ctx.has_full_develop_orchestration_in_window(5));
    }

    #[test]
    fn messages_since_last_and_first() {
        let messages = vec![
            user_msg("a"),
            planner_tool_call(ARCHITECTURE),
            tool_result(ChatMessageType::ToolAnalyzeArchitecture),
            planner_text_only("summary"),
            planner_tool_call(SOLUTION),
            tool_result(ChatMessageType::ToolAnalyzeSolution),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert_eq!(ctx.messages_since_last(SOLUTION), Some(0));
        assert_eq!(ctx.messages_since_last(ARCHITECTURE), Some(3));
        assert_eq!(ctx.messages_since_first(ARCHITECTURE), Some(4));
        assert_eq!(ctx.messages_since_first(SOLUTION), Some(1));
    }

    #[test]
    fn messages_since_last_user_and_touched_since() {
        let messages = vec![
            user_msg("old"),
            planner_tool_call(ARCHITECTURE),
            tool_result(ChatMessageType::ToolAnalyzeArchitecture),
            user_msg("new"),
            planner_tool_call(OBSERVABILITY),
            tool_result(ChatMessageType::ToolAnalyzeObservability),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert_eq!(ctx.messages_since_last_user(), Some(2));
        assert!(ctx.ever_touched_since_last_user(OBSERVABILITY));
        assert!(!ctx.ever_touched_since_last_user(ARCHITECTURE));
    }

    #[test]
    fn planner_ai_turn_count_dedupes_same_message() {
        let messages = vec![
            user_msg("q"),
            planner_tool_calls(&[ARCHITECTURE, OBSERVABILITY]),
            tool_result(ChatMessageType::ToolAnalyzeArchitecture),
            planner_text_only("done"),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert_eq!(ctx.planner_ai_turn_count(), 2);
        assert_eq!(ctx.user_message_count(), 1);
    }

    #[test]
    fn wrong_sequence_order_not_detected() {
        let messages = vec![
            planner_tool_call(SOLUTION),
            tool_result(ChatMessageType::ToolAnalyzeSolution),
            planner_tool_call(ARCHITECTURE),
            tool_result(ChatMessageType::ToolAnalyzeArchitecture),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(!ctx.has_ordered_sequence(&[ARCHITECTURE, SOLUTION], None));
    }

    #[test]
    fn load_latest_releases_tracked_separately() {
        let messages = vec![
            planner_tool_call(LOAD_RELEASES),
            tool_result(ChatMessageType::ToolFetchLatestReleases),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert_eq!(ctx.request_count(LOAD_RELEASES), 1);
        assert_eq!(ctx.result_count(LOAD_RELEASES), 1);
        assert!(!ctx.ever_orchestration_tool());
        assert!(ctx.is_pre_architecture_phase());
    }

    #[test]
    fn solution_without_implement() {
        let messages = vec![
            planner_tool_call(SOLUTION),
            tool_result(ChatMessageType::ToolAnalyzeSolution),
        ];
        let ctx = SessionFlowContext::from_planner_history(&messages);
        assert!(ctx.has_solution_without_following_implement());
        assert!(ctx.ever_solution());
        assert!(!ctx.ever_architecture());
    }
}
