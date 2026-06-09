//! Shared parsing of chat rows into [`FlowHistoryScan`]: assistant `requested_tool_calls` plus
//! AI tool-result message types mapped to canonical tool names. Used by [`crate::services::chat_worker::agent_flow::context::SessionFlowContext`]
//! (planner) and [`SubAgentBranchContext`] (nested agents); extend here as flow rules grow.

use crate::libs::unit_slug::{normalize_full_unit_id_arg, parse_unit_path};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageType, ChatSessionRoleType, TextPayload,
};
use crate::services::chat_worker::agent_flow::discover_units_gate;
use crate::services::chat_worker::llm::tools::{
    analyze_architecture_tool, analyze_observability_tool, analyze_solution_tool, create_unit,
    fetch_release_traces, finish_architecture_analysis, finish_implementation,
    finish_observability_analysis, finish_solution_analysis, implement_solution_tool,
    list_modules_for_unit_contexts, list_project_env_keys, list_unit_contexts,
    get_packages_type_definition_files,
    load_latest_production_release_units,
    load_latest_specific_production_release_units,
    load_latest_releases, load_specific_production_release_units, modify_unit, remove_unit,
};
use crate::services::chat_worker::llm::tools::fetch_session_production_units_common::{
    normalize_dedupe_unit_paths, unit_path_key,
};
use serde_json::Value;
use std::collections::HashSet;

/// `load_latest_production_release_units`, `list_unit_contexts`, and `list_modules_for_unit_contexts`.
/// Nested-agent gates use [`FlowHistoryScan::discover_units_prerequisites_met`], which omits the
/// modules tool when the latest successful **`list_unit_contexts`** payload has empty `context_slugs`.
pub const DISCOVER_UNITS_TRIO: &[&str] = &[
    load_latest_production_release_units::TOOL_NAME,
    list_unit_contexts::TOOL_NAME,
    list_modules_for_unit_contexts::TOOL_NAME,
];

#[derive(Debug, Clone, Default)]
pub struct FlowHistoryScan {
    assistant_requested: HashSet<String>,
    tool_row: HashSet<String>,
    last_successful_list_unit_contexts_was_empty: Option<bool>,
    successfully_loaded_latest_specific_unit_paths: HashSet<String>,
}

impl FlowHistoryScan {
    pub fn record_assistant_tool_name(&mut self, name: &str) {
        self.assistant_requested.insert(name.to_string());
    }

    pub fn record_tool_row_name(&mut self, name: &str) {
        self.tool_row.insert(name.to_string());
    }

    pub fn ever_requested(&self, tool_name: &str) -> bool {
        self.assistant_requested.contains(tool_name)
    }

    pub fn ever_tool_result_row(&self, tool_name: &str) -> bool {
        self.tool_row.contains(tool_name)
    }

    pub fn ever_invoked(&self, tool_name: &str) -> bool {
        self.ever_requested(tool_name) || self.ever_tool_result_row(tool_name)
    }

    pub fn all_invoked(&self, tool_names: &[&str]) -> bool {
        tool_names.iter().all(|t| self.ever_invoked(t))
    }

    pub fn last_list_unit_contexts_had_no_slugs(&self) -> Option<bool> {
        self.last_successful_list_unit_contexts_was_empty
    }

    pub fn unit_path_loaded_via_latest_specific(&self, unit_path: &str) -> bool {
        let key = unit_path.trim().to_ascii_lowercase();
        self.successfully_loaded_latest_specific_unit_paths
            .contains(&key)
    }

    pub fn mutate_prerequisite_for_unit_id_in_args(
        &self,
        tool_name: &str,
        args: &Value,
    ) -> Result<(), String> {
        let Some(unit_id) = args.get("unit_id").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        let unit_id = normalize_full_unit_id_arg(unit_id);
        if unit_id.is_empty() {
            return Ok(());
        }
        let path = parse_unit_path(&unit_id)?;
        let path_key = unit_path_key(&path);
        if self.unit_path_loaded_via_latest_specific(&path_key) {
            Ok(())
        } else {
            Err(discover_units_gate::mutate_unit_locked_until_load_message(
                tool_name, &path_key,
            ))
        }
    }

    pub fn discover_units_prerequisites_met(&self) -> bool {
        if !self.ever_invoked(load_latest_production_release_units::TOOL_NAME)
            || !self.ever_invoked(list_unit_contexts::TOOL_NAME)
        {
            return false;
        }
        let modules_unnecessary = matches!(
            self.last_successful_list_unit_contexts_was_empty,
            Some(true)
        );
        modules_unnecessary || self.ever_invoked(list_modules_for_unit_contexts::TOOL_NAME)
    }

    pub fn ingest_ai_message(
        &mut self,
        role: &ChatSessionRoleType,
        msg_type: &ChatMessageType,
        payload: &Option<Value>,
    ) {
        if *role != ChatSessionRoleType::Ai {
            return;
        }
        if *msg_type == ChatMessageType::Text {
            for n in tool_names_from_text_payload(payload) {
                self.record_assistant_tool_name(&n);
            }
        } else if let Some(name) = tool_name_for_ai_tool_result_type(msg_type) {
            self.record_tool_row_name(name);
            if *msg_type == ChatMessageType::ToolListUnitContexts {
                if let Ok(Some(ChatMessagePayload::ToolListUnitContexts(p))) =
                    ChatMessagePayload::from_value(msg_type, payload.clone())
                {
                    let ok = p
                        .error
                        .as_ref()
                        .is_none_or(|e| e.trim().is_empty());
                    if ok {
                        self.last_successful_list_unit_contexts_was_empty =
                            Some(p.context_slugs.is_empty());
                    }
                }
            } else if *msg_type == ChatMessageType::ToolFetchLatestSpecificProductionReleaseUnits
                && let Ok(Some(
                    ChatMessagePayload::ToolFetchLatestSpecificProductionReleaseUnits(p),
                )) = ChatMessagePayload::from_value(msg_type, payload.clone())
                {
                    let ok = p
                        .error
                        .as_ref()
                        .is_none_or(|e| e.trim().is_empty());
                    if ok {
                        for path in normalize_dedupe_unit_paths(&p.args.unit_paths) {
                            self.successfully_loaded_latest_specific_unit_paths
                                .insert(path);
                        }
                    }
                }
        }
    }

    pub fn from_sub_agent_branch_messages(
        messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    ) -> Self {
        let mut scan = Self::default();
        for (role, typ, payload) in messages {
            scan.ingest_ai_message(role, typ, payload);
        }
        scan
    }
}

pub(crate) fn parse_text_payload(payload: &Option<Value>) -> Option<TextPayload> {
    payload
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

pub(crate) fn tool_names_from_text_payload(payload: &Option<Value>) -> Vec<String> {
    let Some(p) = parse_text_payload(payload) else {
        return Vec::new();
    };
    p.requested_tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|tc| tc.tool_name)
        .collect()
}

pub fn tool_name_for_ai_tool_result_type(typ: &ChatMessageType) -> Option<&'static str> {
    Some(match typ {
        ChatMessageType::ToolAnalyzeArchitecture => analyze_architecture_tool::TOOL_NAME,
        ChatMessageType::ToolAnalyzeSolution => analyze_solution_tool::TOOL_NAME,
        ChatMessageType::ToolImplementSolution => implement_solution_tool::TOOL_NAME,
        ChatMessageType::ToolAnalyzeObservability => analyze_observability_tool::TOOL_NAME,
        ChatMessageType::ToolFetchLatestReleases => load_latest_releases::TOOL_NAME,
        ChatMessageType::ToolFetchRelevantUnits => load_latest_production_release_units::TOOL_NAME,
        ChatMessageType::ToolFetchSpecificProductionReleaseUnits => {
            load_specific_production_release_units::TOOL_NAME
        }
        ChatMessageType::ToolFetchLatestSpecificProductionReleaseUnits => {
            load_latest_specific_production_release_units::TOOL_NAME
        }
        ChatMessageType::ToolListUnitContexts => list_unit_contexts::TOOL_NAME,
        ChatMessageType::ToolListProjectEnvKeys => list_project_env_keys::TOOL_NAME,
        ChatMessageType::ToolGetPackagesTypeDefinitionFiles => {
            get_packages_type_definition_files::TOOL_NAME
        }
        ChatMessageType::ToolListModulesForUnitContexts => {
            list_modules_for_unit_contexts::TOOL_NAME
        }
        ChatMessageType::ToolFetchReleaseTraces => fetch_release_traces::TOOL_NAME,
        ChatMessageType::ToolFinishArchitectureAnalysis => {
            finish_architecture_analysis::TOOL_NAME
        }
        ChatMessageType::ToolFinishSolutionAnalysis => finish_solution_analysis::TOOL_NAME,
        ChatMessageType::ToolFinishObservabilityAnalysis => {
            finish_observability_analysis::TOOL_NAME
        }
        ChatMessageType::ToolFinishImplementation => finish_implementation::TOOL_NAME,
        ChatMessageType::ToolCreateUnit => create_unit::TOOL_NAME,
        ChatMessageType::ToolModifyUnit => modify_unit::TOOL_NAME,
        ChatMessageType::ToolRemoveUnit => remove_unit::TOOL_NAME,
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct SubAgentBranchContext(FlowHistoryScan);

impl SubAgentBranchContext {
    pub fn from_messages(
        messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    ) -> Self {
        Self(FlowHistoryScan::from_sub_agent_branch_messages(messages))
    }

    pub fn scan(&self) -> &FlowHistoryScan {
        &self.0
    }
}

impl std::ops::Deref for SubAgentBranchContext {
    type Target = FlowHistoryScan;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::chat_messages::RequestedToolCall;
    use crate::services::chat_worker::llm::tools::list_unit_contexts::ToolListUnitContextsPayload;
    use crate::services::chat_worker::llm::tools::load_latest_specific_production_release_units::{
        ToolFetchLatestSpecificProductionReleaseUnitsArgs,
        ToolFetchLatestSpecificProductionReleaseUnitsPayload,
    };

    fn user_text() -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        (
            ChatSessionRoleType::User,
            ChatMessageType::Text,
            Some(serde_json::json!({ "text": "hello" })),
        )
    }

    fn system_text() -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        (
            ChatSessionRoleType::System,
            ChatMessageType::Text,
            Some(serde_json::json!({ "text": "notice" })),
        )
    }

    fn ai_text_tools(tool_names: &[&str]) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        let payload = TextPayload {
            text: String::new(),
            requested_tool_calls: Some(
                tool_names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| RequestedToolCall {
                        tool_call_id: Some(format!("{i}")),
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

    fn ai_tool_row(toolbar: ChatMessageType) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        (ChatSessionRoleType::Ai, toolbar, None)
    }

    fn ai_list_unit_contexts_row(
        context_slugs: Vec<String>,
        error: Option<String>,
    ) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        let p = ToolListUnitContextsPayload {
            args: Default::default(),
            context_slugs,
            tool_call_id: None,
            tool_name: Some(list_unit_contexts::TOOL_NAME.to_string()),
            error,
        };
        (
            ChatSessionRoleType::Ai,
            ChatMessageType::ToolListUnitContexts,
            Some(serde_json::to_value(&p).unwrap()),
        )
    }

    fn ai_text_no_calls() -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        let payload = TextPayload {
            text: "only prose".into(),
            requested_tool_calls: None,
            thinking_blocks: None,
        };
        (
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(serde_json::to_value(&payload).unwrap()),
        )
    }

    fn ai_text_empty_calls() -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        let payload = TextPayload {
            text: String::new(),
            requested_tool_calls: Some(vec![]),
            thinking_blocks: None,
        };
        (
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(serde_json::to_value(&payload).unwrap()),
        )
    }

    #[test]
    fn empty_history() {
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&[]);
        assert!(!scan.ever_invoked(fetch_release_traces::TOOL_NAME));
        assert!(!scan.all_invoked(DISCOVER_UNITS_TRIO));
    }

    #[test]
    fn all_invoked_empty_slice_is_vacuously_true() {
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&[]);
        assert!(scan.all_invoked(&[]));
    }

    #[test]
    fn user_and_system_rows_do_not_add_tools() {
        let messages = vec![
            user_text(),
            ai_text_tools(&[list_unit_contexts::TOOL_NAME]),
            system_text(),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.ever_invoked(list_unit_contexts::TOOL_NAME));
        assert!(!scan.ever_invoked(load_latest_production_release_units::TOOL_NAME));
    }

    #[test]
    fn ai_text_without_requested_tools_records_nothing() {
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&[
            ai_text_no_calls(),
            ai_text_empty_calls(),
        ]);
        assert!(!scan.ever_invoked(list_unit_contexts::TOOL_NAME));
    }

    #[test]
    fn ai_text_none_payload_skipped() {
        let row = (ChatSessionRoleType::Ai, ChatMessageType::Text, None);
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&[row]);
        assert!(!scan.ever_invoked(list_unit_contexts::TOOL_NAME));
    }

    #[test]
    fn ai_text_invalid_payload_skipped() {
        let row = (
            ChatSessionRoleType::Ai,
            ChatMessageType::Text,
            Some(serde_json::json!("not-an-object")),
        );
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&[row]);
        assert!(!scan.ever_invoked(list_unit_contexts::TOOL_NAME));
    }

    #[test]
    fn unmapped_tool_result_type_does_not_record() {
        let messages = vec![
            ai_tool_row(ChatMessageType::Units),
            ai_tool_row(ChatMessageType::ReleaseControllerBox),
            ai_tool_row(ChatMessageType::RejectControllerBox),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(!scan.ever_invoked(load_latest_production_release_units::TOOL_NAME));
        assert!(!scan.ever_invoked(analyze_architecture_tool::TOOL_NAME));
    }

    #[test]
    fn duplicate_invocations_collapse_for_membership() {
        let messages = vec![
            ai_text_tools(&[list_unit_contexts::TOOL_NAME]),
            ai_text_tools(&[list_unit_contexts::TOOL_NAME]),
            ai_tool_row(ChatMessageType::ToolListUnitContexts),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.ever_invoked(list_unit_contexts::TOOL_NAME));
        assert!(!scan.ever_invoked(list_modules_for_unit_contexts::TOOL_NAME));
    }

    #[test]
    fn multiple_parallel_tool_calls_in_one_turn() {
        let messages = vec![ai_text_tools(&[
            load_latest_production_release_units::TOOL_NAME,
            list_unit_contexts::TOOL_NAME,
            fetch_release_traces::TOOL_NAME,
        ])];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.all_invoked(&[
            load_latest_production_release_units::TOOL_NAME,
            list_unit_contexts::TOOL_NAME,
        ]));
        assert!(scan.ever_invoked(fetch_release_traces::TOOL_NAME));
    }

    #[test]
    fn discover_trio_requests_without_result_rows() {
        let messages = vec![ai_text_tools(DISCOVER_UNITS_TRIO)];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.all_invoked(DISCOVER_UNITS_TRIO));
    }

    #[test]
    fn discover_trio_mixed_request_and_result_rows() {
        let messages = vec![
            ai_text_tools(&[load_latest_production_release_units::TOOL_NAME]),
            ai_tool_row(ChatMessageType::ToolListUnitContexts),
            ai_tool_row(ChatMessageType::ToolListModulesForUnitContexts),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.all_invoked(DISCOVER_UNITS_TRIO));
    }

    #[test]
    fn all_invoked_false_when_one_missing() {
        let messages = vec![
            ai_tool_row(ChatMessageType::ToolFetchRelevantUnits),
            ai_tool_row(ChatMessageType::ToolListUnitContexts),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(!scan.all_invoked(DISCOVER_UNITS_TRIO));
    }

    #[test]
    fn discover_prerequisites_met_without_modules_when_no_contexts() {
        let messages = vec![
            ai_tool_row(ChatMessageType::ToolFetchRelevantUnits),
            ai_list_unit_contexts_row(vec![], None),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.discover_units_prerequisites_met());
        assert!(!scan.ever_invoked(list_modules_for_unit_contexts::TOOL_NAME));
    }

    #[test]
    fn discover_prerequisites_not_met_without_modules_when_contexts_exist() {
        let messages = vec![
            ai_tool_row(ChatMessageType::ToolFetchRelevantUnits),
            ai_list_unit_contexts_row(vec!["billing".into()], None),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(!scan.discover_units_prerequisites_met());
    }

    #[test]
    fn discover_prerequisites_ignores_list_contexts_row_with_error_for_empty_skip() {
        let messages = vec![
            ai_tool_row(ChatMessageType::ToolFetchRelevantUnits),
            ai_list_unit_contexts_row(vec![], Some("db".into())),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(!scan.discover_units_prerequisites_met());
    }

    #[test]
    fn discover_prerequisites_non_empty_after_empty_requires_modules() {
        let messages = vec![
            ai_tool_row(ChatMessageType::ToolFetchRelevantUnits),
            ai_list_unit_contexts_row(vec![], None),
            ai_list_unit_contexts_row(vec!["a".into()], None),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(!scan.discover_units_prerequisites_met());
    }

    #[test]
    fn unknown_tool_name_from_request_is_tracked() {
        let messages = vec![ai_text_tools(&["tool_that_does_not_exist_in_registry"])];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.ever_invoked("tool_that_does_not_exist_in_registry"));
        assert!(!scan.ever_invoked(list_unit_contexts::TOOL_NAME));
    }

    #[test]
    fn all_mapped_finish_and_unit_crud_result_types() {
        let messages = vec![
            ai_tool_row(ChatMessageType::ToolCreateUnit),
            ai_tool_row(ChatMessageType::ToolModifyUnit),
            ai_tool_row(ChatMessageType::ToolRemoveUnit),
            ai_tool_row(ChatMessageType::ToolFinishImplementation),
            ai_tool_row(ChatMessageType::ToolFinishArchitectureAnalysis),
            ai_tool_row(ChatMessageType::ToolFinishSolutionAnalysis),
            ai_tool_row(ChatMessageType::ToolFinishObservabilityAnalysis),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.ever_invoked(create_unit::TOOL_NAME));
        assert!(scan.ever_invoked(modify_unit::TOOL_NAME));
        assert!(scan.ever_invoked(remove_unit::TOOL_NAME));
        assert!(scan.ever_invoked(finish_implementation::TOOL_NAME));
        assert!(scan.ever_invoked(finish_architecture_analysis::TOOL_NAME));
        assert!(scan.ever_invoked(finish_solution_analysis::TOOL_NAME));
        assert!(scan.ever_invoked(finish_observability_analysis::TOOL_NAME));
    }

    #[test]
    fn detects_requested_and_result_tools() {
        let messages = vec![
            ai_text_tools(&[fetch_release_traces::TOOL_NAME]),
            ai_tool_row(ChatMessageType::ToolFetchReleaseTraces),
            ai_text_tools(&[load_latest_production_release_units::TOOL_NAME]),
            ai_tool_row(ChatMessageType::ToolFetchRelevantUnits),
        ];
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&messages);
        assert!(scan.ever_invoked(fetch_release_traces::TOOL_NAME));
        assert!(scan.ever_invoked(load_latest_production_release_units::TOOL_NAME));
        assert!(!scan.ever_invoked(list_unit_contexts::TOOL_NAME));
    }

    #[test]
    fn observability_traces_before_load_gate() {
        let scan_fetch_only = FlowHistoryScan::from_sub_agent_branch_messages(&[ai_tool_row(
            ChatMessageType::ToolFetchReleaseTraces,
        )]);
        assert!(scan_fetch_only.ever_invoked(fetch_release_traces::TOOL_NAME));
        assert!(!scan_fetch_only.ever_invoked(load_latest_production_release_units::TOOL_NAME));

        let scan_both = FlowHistoryScan::from_sub_agent_branch_messages(&[
            ai_tool_row(ChatMessageType::ToolFetchReleaseTraces),
            ai_tool_row(ChatMessageType::ToolFetchRelevantUnits),
        ]);
        assert!(scan_both.ever_invoked(fetch_release_traces::TOOL_NAME));
        assert!(scan_both.ever_invoked(load_latest_production_release_units::TOOL_NAME));
    }

    fn ai_latest_specific_load_row(
        unit_paths: Vec<String>,
        error: Option<String>,
    ) -> (ChatSessionRoleType, ChatMessageType, Option<Value>) {
        let p = ToolFetchLatestSpecificProductionReleaseUnitsPayload {
            args: ToolFetchLatestSpecificProductionReleaseUnitsArgs { unit_paths },
            units: vec![],
            tool_call_id: None,
            tool_name: Some(load_latest_specific_production_release_units::TOOL_NAME.to_string()),
            error,
        };
        (
            ChatSessionRoleType::Ai,
            ChatMessageType::ToolFetchLatestSpecificProductionReleaseUnits,
            Some(serde_json::to_value(&p).unwrap()),
        )
    }

    #[test]
    fn mutate_prerequisite_requires_successful_latest_specific_load_for_path() {
        let unit_id = "demo.greeter.api.greeter-api.1";
        let path = "demo.greeter.api.greeter-api";
        let scan_empty = FlowHistoryScan::from_sub_agent_branch_messages(&[]);
        assert!(
            scan_empty
                .mutate_prerequisite_for_unit_id_in_args(
                    modify_unit::TOOL_NAME,
                    &serde_json::json!({ "unit_id": unit_id }),
                )
                .is_err()
        );

        let scan_loaded = FlowHistoryScan::from_sub_agent_branch_messages(&[
            ai_latest_specific_load_row(vec![path.into()], None),
        ]);
        assert!(
            scan_loaded
                .mutate_prerequisite_for_unit_id_in_args(
                    remove_unit::TOOL_NAME,
                    &serde_json::json!({ "unit_id": unit_id }),
                )
                .is_ok()
        );
    }

    #[test]
    fn mutate_prerequisite_ignores_failed_latest_specific_load() {
        let unit_id = "demo.greeter.api.greeter-api.1";
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&[ai_latest_specific_load_row(
            vec!["demo.greeter.api.greeter-api".into()],
            Some("No unit found".into()),
        )]);
        assert!(
            scan.mutate_prerequisite_for_unit_id_in_args(
                modify_unit::TOOL_NAME,
                &serde_json::json!({ "unit_id": unit_id }),
            )
            .is_err()
        );
    }

    #[test]
    fn mutate_prerequisite_accepts_path_in_multi_path_load() {
        let scan = FlowHistoryScan::from_sub_agent_branch_messages(&[ai_latest_specific_load_row(
            vec![
                "auth.users.api.users".into(),
                "demo.greeter.api.greeter-api".into(),
            ],
            None,
        )]);
        assert!(
            scan.mutate_prerequisite_for_unit_id_in_args(
                modify_unit::TOOL_NAME,
                &serde_json::json!({ "unit_id": "demo.greeter.api.greeter-api.2" }),
            )
            .is_ok()
        );
        assert!(
            scan.mutate_prerequisite_for_unit_id_in_args(
                modify_unit::TOOL_NAME,
                &serde_json::json!({ "unit_id": "billing.core.api.invoice.1" }),
            )
            .is_err()
        );
    }

    #[test]
    fn tool_name_mapping_covers_planner_async_rows() {
        assert_eq!(
            tool_name_for_ai_tool_result_type(&ChatMessageType::ToolAnalyzeArchitecture),
            Some(analyze_architecture_tool::TOOL_NAME)
        );
        assert_eq!(
            tool_name_for_ai_tool_result_type(&ChatMessageType::ToolFetchLatestReleases),
            Some(load_latest_releases::TOOL_NAME)
        );
    }
}
