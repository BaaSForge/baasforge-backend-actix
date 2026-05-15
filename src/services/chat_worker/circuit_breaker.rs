use crate::models::chat_messages::{
    ChatAgentType, ChatMessageRow, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
};
use crate::services::chat_worker::agent::SessionHistoryLoad;
use crate::services::chat_worker::agent_flow::tool_name_for_ai_tool_result_type;
use serde_json::Value;

pub const SAME_TOOL_FAILURE_STREAK_LEN: usize = 5;

pub const MAX_PLANNER_TURN_MESSAGES: usize = 100;

pub const MAX_SUBAGENT_THREAD_MESSAGES: usize = 100;

pub const MAX_FAILED_TOOL_INVOCATIONS_PER_TURN: usize = 20;

pub const SAME_TOOL_FAILURE_STREAK_MESSAGE: &str = "The assistant hit the same failing tool several times in a row, so we stopped the run to avoid a loop. Please rephrase what you want, adjust the goal, or try again in a moment.";

pub const MAX_THREAD_MESSAGES_MESSAGE: &str = "This turn exceeded the maximum conversation size we allow, so we stopped. Continue with a shorter request or a new message.";

pub const MAX_FAILED_TOOLS_MESSAGE: &str = "This turn had too many failed tool attempts, so we stopped. Adjust what you are asking for or try again.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitBreakerScope {
    PlannerTurnAfterLastUser,
    NestedSubagentBranch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitBreakerKind {
    MaxMessagesInTurn,
    MaxFailedToolsInTurn,
    SameToolFailureStreak,
}

impl CircuitBreakerKind {
    pub fn user_message(self) -> &'static str {
        match self {
            CircuitBreakerKind::SameToolFailureStreak => SAME_TOOL_FAILURE_STREAK_MESSAGE,
            CircuitBreakerKind::MaxMessagesInTurn => MAX_THREAD_MESSAGES_MESSAGE,
            CircuitBreakerKind::MaxFailedToolsInTurn => MAX_FAILED_TOOLS_MESSAGE,
        }
    }

    pub fn cascade_error(self) -> &'static str {
        self.user_message()
    }
}

pub fn evaluate_before_llm(agent: &ChatAgentType, load: &SessionHistoryLoad) -> Option<CircuitBreakerKind> {
    let db = &load.db_rows_chronological;
    match agent {
        ChatAgentType::Planner => {
            let turn = segment_for_scope(db, CircuitBreakerScope::PlannerTurnAfterLastUser);
            if turn.len() > MAX_PLANNER_TURN_MESSAGES {
                return Some(CircuitBreakerKind::MaxMessagesInTurn);
            }
            if count_failed_tool_invocations(turn) > MAX_FAILED_TOOL_INVOCATIONS_PER_TURN {
                return Some(CircuitBreakerKind::MaxFailedToolsInTurn);
            }
            if same_tool_failure_streak_tripped(db, CircuitBreakerScope::PlannerTurnAfterLastUser).is_some() {
                return Some(CircuitBreakerKind::SameToolFailureStreak);
            }
        }
        ChatAgentType::ArchitectureAnalyzer
        | ChatAgentType::SolutionAnalyzer
        | ChatAgentType::Developer
        | ChatAgentType::ObservabilityAnalyzer => {
            if load.session_messages.len() > MAX_SUBAGENT_THREAD_MESSAGES {
                return Some(CircuitBreakerKind::MaxMessagesInTurn);
            }
            let branch = segment_for_scope(db, CircuitBreakerScope::NestedSubagentBranch);
            if count_failed_tool_invocations(branch) > MAX_FAILED_TOOL_INVOCATIONS_PER_TURN {
                return Some(CircuitBreakerKind::MaxFailedToolsInTurn);
            }
            if same_tool_failure_streak_tripped(db, CircuitBreakerScope::NestedSubagentBranch).is_some() {
                return Some(CircuitBreakerKind::SameToolFailureStreak);
            }
        }
    }
    None
}

fn count_failed_tool_invocations(rows: &[ChatMessageRow]) -> usize {
    rows.iter().filter(|r| failed_invocation_tool_name(r).is_some()).count()
}

fn payload_has_actionable_error(payload: &Option<Value>) -> bool {
    let Some(v) = payload else {
        return false;
    };
    match v.get("error") {
        None => false,
        Some(e) if e.is_null() => false,
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => false,
        Some(_) => true,
    }
}

fn failed_invocation_tool_name(row: &ChatMessageRow) -> Option<&str> {
    if row.role != ChatSessionRoleType::Ai {
        return None;
    }
    let failed_status = row.status == ChatMessageStatus::Failed;
    let completed_with_error =
        row.status == ChatMessageStatus::Completed && payload_has_actionable_error(&row.payload);
    if !failed_status && !completed_with_error {
        return None;
    }
    match row.type_ {
        ChatMessageType::Text => row
            .payload
            .as_ref()
            .and_then(|v| v.get("tool_name"))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        _ => tool_name_for_ai_tool_result_type(&row.type_),
    }
}

fn succeeded_tool_invocation(row: &ChatMessageRow) -> bool {
    row.role == ChatSessionRoleType::Ai
        && row.type_ != ChatMessageType::Text
        && row.status == ChatMessageStatus::Completed
        && !payload_has_actionable_error(&row.payload)
}

fn segment_for_scope(chrono_db_rows: &[ChatMessageRow], scope: CircuitBreakerScope) -> &[ChatMessageRow] {
    match scope {
        CircuitBreakerScope::NestedSubagentBranch => chrono_db_rows,
        CircuitBreakerScope::PlannerTurnAfterLastUser => {
            let idx = chrono_db_rows
                .iter()
                .rposition(|r| r.role == ChatSessionRoleType::User);
            match idx {
                Some(i) if i + 1 < chrono_db_rows.len() => &chrono_db_rows[i + 1..],
                Some(_) => &[],
                None => chrono_db_rows,
            }
        }
    }
}

pub fn same_tool_failure_streak_tripped<'a>(
    chrono_db_rows: &'a [ChatMessageRow],
    scope: CircuitBreakerScope,
) -> Option<&'a str> {
    let segment = segment_for_scope(chrono_db_rows, scope);
    let mut streak_tool: Option<&'a str> = None;
    let mut streak_len = 0usize;
    let mut tripped: Option<&'a str> = None;
    for row in segment {
        if row.type_ == ChatMessageType::Text {
            continue;
        }
        if let Some(name) = failed_invocation_tool_name(row) {
            if streak_tool == Some(name) {
                streak_len += 1;
            } else {
                streak_tool = Some(name);
                streak_len = 1;
            }
            if streak_len >= SAME_TOOL_FAILURE_STREAK_LEN {
                tripped = Some(name);
            }
        } else if succeeded_tool_invocation(row) {
            streak_tool = None;
            streak_len = 0;
        } else {
            streak_tool = None;
            streak_len = 0;
        }
    }
    tripped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libs::ids::new_uuid;
    use crate::models::chat_messages::{ChatAgentType, ChatMessageStatus, ChatMessageType, ChatSessionRoleType};
    use crate::services::chat_worker::llm::tools::{fetch_release_traces, list_unit_contexts};
    use chrono::Utc;

    fn row(
        role: ChatSessionRoleType,
        status: ChatMessageStatus,
        type_: ChatMessageType,
        payload: Option<Value>,
    ) -> ChatMessageRow {
        ChatMessageRow {
            id: new_uuid(),
            chat_session_id: new_uuid(),
            role,
            type_,
            payload,
            status,
            created_at: Utc::now(),
            invoking_message_id: None,
            agent: Some(ChatAgentType::Planner),
            is_edit_disabled_due_to_live_production_release: false,
        }
    }

    fn ai_tool_failed(type_: ChatMessageType, err: &str) -> ChatMessageRow {
        row(
            ChatSessionRoleType::Ai,
            ChatMessageStatus::Failed,
            type_,
            Some(serde_json::json!({"error": err})),
        )
    }

    fn user_text() -> ChatMessageRow {
        row(
            ChatSessionRoleType::User,
            ChatMessageStatus::Completed,
            ChatMessageType::Text,
            Some(serde_json::json!({"text": "hi"})),
        )
    }

    fn assistant_text_ok() -> ChatMessageRow {
        row(
            ChatSessionRoleType::Ai,
            ChatMessageStatus::Completed,
            ChatMessageType::Text,
            Some(serde_json::json!({"text": "thinking"})),
        )
    }

    fn ai_list_unit_contexts_completed_ok() -> ChatMessageRow {
        row(
            ChatSessionRoleType::Ai,
            ChatMessageStatus::Completed,
            ChatMessageType::ToolListUnitContexts,
            Some(serde_json::json!({"args": {}})),
        )
    }

    #[test]
    fn planner_five_same_tool_failures_after_last_user_trips() {
        let rows = vec![
            user_text(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "a"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "b"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "c"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "d"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "e"),
        ];
        let t = same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::PlannerTurnAfterLastUser);
        assert_eq!(t, Some(list_unit_contexts::TOOL_NAME));
    }

    #[test]
    fn planner_old_failures_before_user_ignored() {
        let rows = vec![
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "old"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "old"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "old"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "old"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "old"),
            user_text(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "a"),
            ai_tool_failed(ChatMessageType::ToolListModulesForUnitContexts, "b"),
        ];
        assert!(same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::PlannerTurnAfterLastUser).is_none());
    }

    #[test]
    fn nested_five_same_tool_failures_trips_without_user_row() {
        let rows: Vec<_> = (0..5)
            .map(|_| ai_tool_failed(ChatMessageType::ToolFetchReleaseTraces, "x"))
            .collect();
        let t = same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch);
        assert_eq!(t, Some(fetch_release_traces::TOOL_NAME));
    }

    #[test]
    fn assistant_text_between_failures_does_not_reset_streak() {
        let rows = vec![
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "1"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "2"),
            assistant_text_ok(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "3"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "4"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "5"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "6"),
        ];
        assert_eq!(
            same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch),
            Some(list_unit_contexts::TOOL_NAME)
        );
    }

    #[test]
    fn text_weaving_same_pattern_as_user_scenario_trips() {
        let rows = vec![
            assistant_text_ok(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "1"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "2"),
            assistant_text_ok(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "3"),
            assistant_text_ok(),
            assistant_text_ok(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "4"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "5"),
        ];
        assert_eq!(
            same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch),
            Some(list_unit_contexts::TOOL_NAME)
        );
    }

    #[test]
    fn five_consecutive_failures_after_other_message_trips() {
        let rows = vec![
            assistant_text_ok(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "1"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "2"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "3"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "4"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "5"),
        ];
        assert_eq!(
            same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch),
            Some(list_unit_contexts::TOOL_NAME)
        );
    }

    #[test]
    fn four_failures_does_not_trip() {
        let rows: Vec<_> = (0..4)
            .map(|_| ai_tool_failed(ChatMessageType::ToolListUnitContexts, "e"))
            .collect();
        assert!(same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch).is_none());
    }

    #[test]
    fn five_failures_alternating_tools_does_not_trip() {
        let rows = vec![
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "1"),
            ai_tool_failed(ChatMessageType::ToolListModulesForUnitContexts, "2"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "3"),
            ai_tool_failed(ChatMessageType::ToolListModulesForUnitContexts, "4"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "5"),
        ];
        assert!(same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch).is_none());
    }

    #[test]
    fn completed_with_error_counts_as_failed() {
        let rows: Vec<_> = (0..5)
            .map(|_| {
                row(
                    ChatSessionRoleType::Ai,
                    ChatMessageStatus::Completed,
                    ChatMessageType::ToolListUnitContexts,
                    Some(serde_json::json!({"error": "validation"})),
                )
            })
            .collect();
        assert_eq!(
            same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch),
            Some(list_unit_contexts::TOOL_NAME)
        );
    }

    #[test]
    fn successful_same_tool_between_failure_blocks_does_not_trip() {
        let mut rows: Vec<_> = (0..4)
            .map(|_| ai_tool_failed(ChatMessageType::ToolListUnitContexts, "a"))
            .collect();
        rows.push(ai_list_unit_contexts_completed_ok());
        rows.extend((0..4).map(|_| ai_tool_failed(ChatMessageType::ToolListUnitContexts, "b")));
        assert!(same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch).is_none());
    }

    #[test]
    fn successful_same_tool_after_two_failures_resets_so_tail_of_four_fails_no_trip() {
        let rows = vec![
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "1"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "2"),
            ai_list_unit_contexts_completed_ok(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "3"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "4"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "5"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "6"),
        ];
        assert!(same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch).is_none());
    }

    #[test]
    fn successful_tool_breaks_tail() {
        let rows = vec![
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "1"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "2"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "3"),
            ai_list_unit_contexts_completed_ok(),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "4"),
            ai_tool_failed(ChatMessageType::ToolListUnitContexts, "5"),
        ];
        assert!(same_tool_failure_streak_tripped(&rows, CircuitBreakerScope::NestedSubagentBranch).is_none());
    }

    #[test]
    fn planner_turn_over_max_db_messages_trips() {
        let mut db = vec![user_text()];
        db.extend((0..101).map(|_| assistant_text_ok()));
        let load = SessionHistoryLoad {
            session_messages: vec![],
            db_rows_chronological: db,
        };
        assert_eq!(
            evaluate_before_llm(&ChatAgentType::Planner, &load),
            Some(CircuitBreakerKind::MaxMessagesInTurn)
        );
    }

    #[test]
    fn nested_thread_over_max_session_messages_trips() {
        let session_messages: Vec<_> = (0..101)
            .map(|_| {
                (
                    ChatSessionRoleType::Ai,
                    ChatMessageType::Text,
                    Some(serde_json::json!({"text": "x"})),
                )
            })
            .collect();
        let load = SessionHistoryLoad {
            session_messages,
            db_rows_chronological: vec![],
        };
        assert_eq!(
            evaluate_before_llm(&ChatAgentType::ArchitectureAnalyzer, &load),
            Some(CircuitBreakerKind::MaxMessagesInTurn)
        );
    }

    #[test]
    fn planner_turn_over_max_failed_tools_trips() {
        let mut db = vec![user_text()];
        db.extend((0..21).map(|_| ai_tool_failed(ChatMessageType::ToolListUnitContexts, "e")));
        let load = SessionHistoryLoad {
            session_messages: vec![],
            db_rows_chronological: db,
        };
        assert_eq!(
            evaluate_before_llm(&ChatAgentType::Planner, &load),
            Some(CircuitBreakerKind::MaxFailedToolsInTurn)
        );
    }

    #[test]
    fn nested_branch_over_max_failed_tools_trips() {
        let rows: Vec<_> = (0..21)
            .map(|_| ai_tool_failed(ChatMessageType::ToolListUnitContexts, "e"))
            .collect();
        let load = SessionHistoryLoad {
            session_messages: vec![(
                ChatSessionRoleType::User,
                ChatMessageType::Text,
                Some(serde_json::json!({"text": "intent"})),
            )],
            db_rows_chronological: rows,
        };
        assert_eq!(
            evaluate_before_llm(&ChatAgentType::Developer, &load),
            Some(CircuitBreakerKind::MaxFailedToolsInTurn)
        );
    }

    #[test]
    fn max_messages_checked_before_failed_tool_count() {
        let mut db = vec![user_text()];
        db.extend((0..101).map(|_| assistant_text_ok()));
        db.extend((0..21).map(|_| ai_tool_failed(ChatMessageType::ToolListUnitContexts, "e")));
        let load = SessionHistoryLoad {
            session_messages: vec![],
            db_rows_chronological: db,
        };
        assert_eq!(
            evaluate_before_llm(&ChatAgentType::Planner, &load),
            Some(CircuitBreakerKind::MaxMessagesInTurn)
        );
    }
}
