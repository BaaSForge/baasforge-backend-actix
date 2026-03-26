// Gemini: streaming + tools via gemini-rust. Thinking / thought summaries:
// https://ai.google.dev/gemini-api/docs/thinking
// Crate: `with_thinking_budget` / `with_thinking_level`, `with_thoughts_included`, stream `text_with_thoughts()`.
use crate::models::chat_messages::{
    ChatAgentType, ChatMessageType, ChatSessionRoleType, RequestedToolCall, TextPayload,
    ThinkingBlock,
};
use crate::routes::ws::events::{
    ChatSessionNewMessageChunkPayload, ChatSessionNewThinkingChunkPayload, WsServerEvent,
};
use crate::routes::ws::{is_message_stopped, publish_event_to_channel};
use crate::libs::ids::new_uuid;
use crate::services::chat_worker::agent::system_prompt_for_agent;
use crate::services::chat_worker::ai_message;
use crate::services::chat_worker::llm::history::{
    session_messages_to_upstream_entries, UpstreamHistoryEntry,
};
use crate::services::chat_worker::llm::GenerateOutcome;
use gemini_rust::client::GenerationStream;
use gemini_rust::tools::model::{FunctionCall, FunctionCallingMode, FunctionDeclaration};
use gemini_rust::{Content, Gemini, Message, Part, Role, ThinkingLevel};
use redis::aio::MultiplexedConnection;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::tools::{
    self, analyze_architecture_tool, analyze_solution_tool, create_unit, finish_architecture_analysis,
    finish_implementation, finish_solution_analysis, implement_solution_tool,
    list_modules_for_unit_contexts, list_unit_contexts, load_latest_production_release_units,
    load_latest_releases, modify_unit, remove_unit,
};
use super::{BoxError, GeminiThinkingEnv};
use crate::services::chat_worker::agent::tool_names_for_agent;
use serde_json::Value;

fn gemini_declaration_fns_by_name() -> &'static [(
    &'static str,
    fn() -> gemini_rust::tools::model::FunctionDeclaration,
)] {
    &[
        (
            load_latest_production_release_units::TOOL_NAME,
            load_latest_production_release_units::gemini_declaration,
        ),
        (
            load_latest_releases::TOOL_NAME,
            load_latest_releases::gemini_declaration,
        ),
        (
            list_unit_contexts::TOOL_NAME,
            list_unit_contexts::gemini_declaration,
        ),
        (
            list_modules_for_unit_contexts::TOOL_NAME,
            list_modules_for_unit_contexts::gemini_declaration,
        ),
        (
            analyze_architecture_tool::TOOL_NAME,
            analyze_architecture_tool::gemini_declaration,
        ),
        (
            finish_architecture_analysis::TOOL_NAME,
            finish_architecture_analysis::gemini_declaration,
        ),
        (
            analyze_solution_tool::TOOL_NAME,
            analyze_solution_tool::gemini_declaration,
        ),
        (
            finish_solution_analysis::TOOL_NAME,
            finish_solution_analysis::gemini_declaration,
        ),
        (
            implement_solution_tool::TOOL_NAME,
            implement_solution_tool::gemini_declaration,
        ),
        (
            finish_implementation::TOOL_NAME,
            finish_implementation::gemini_declaration,
        ),
        (
            create_unit::TOOL_NAME,
            create_unit::gemini_declaration,
        ),
        (
            modify_unit::TOOL_NAME,
            modify_unit::gemini_declaration,
        ),
        (
            remove_unit::TOOL_NAME,
            remove_unit::gemini_declaration,
        ),
    ]
}

/// Gemini declarations for the given agent only (subset of all tools).
fn to_gemini_declarations_for_agent(
    agent: &ChatAgentType,
) -> Vec<gemini_rust::tools::model::FunctionDeclaration> {
    let names = tool_names_for_agent(agent);
    gemini_declaration_fns_by_name()
        .iter()
        .filter(|(name, _)| names.contains(name))
        .map(|(_, f)| f())
        .collect()
}

/// `gemini-rust` builds parameter JSON Schema with schemars OpenAPI settings, which includes
/// `additionalProperties`. The Gemini REST API rejects that key on `function_declarations[].parameters`.
fn strip_additional_properties_in_schema(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("additionalProperties");
            for child in map.values_mut() {
                strip_additional_properties_in_schema(child);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                strip_additional_properties_in_schema(item);
            }
        }
        _ => {}
    }
}

fn sanitize_gemini_function_declaration(
    decl: FunctionDeclaration,
) -> Result<FunctionDeclaration, BoxError> {
    let mut v = serde_json::to_value(&decl).map_err(|e| -> BoxError { e.to_string().into() })?;
    if let Some(params) = v.get_mut("parameters") {
        strip_additional_properties_in_schema(params);
    }
    if let Some(resp) = v.get_mut("response") {
        strip_additional_properties_in_schema(resp);
    }
    serde_json::from_value(v).map_err(|e| -> BoxError { e.to_string().into() })
}

fn gemini_model_id(model: &str) -> String {
    if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{}", model)
    }
}

fn parse_gemini_thinking_level(s: &str) -> Result<ThinkingLevel, BoxError> {
    match s.trim().to_lowercase().as_str() {
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "unspecified" => Ok(ThinkingLevel::ThinkingLevelUnspecified),
        other => Err(format!(
            "GEMINI_THINKING_LEVEL: unknown {:?}; use minimal, low, medium, high, or unspecified",
            other
        )
        .into()),
    }
}

/// One consolidated block from streamed thought summaries (signature when the API attaches it).
fn thinking_blocks_from_gemini_stream(
    full_thinking: &str,
    signature: Option<String>,
) -> Option<Vec<ThinkingBlock>> {
    if full_thinking.is_empty() {
        return None;
    }
    let sig = signature.filter(|s| !s.is_empty());
    Some(vec![ThinkingBlock {
        thinking: full_thinking.to_string(),
        signature: sig,
    }])
}

pub async fn call_gemini_stream(
    api_key: &str,
    model: &str,
    thinking: &GeminiThinkingEnv,
    pool: &crate::database::DbPool,
    session_messages: &[(ChatSessionRoleType, ChatMessageType, Option<Value>)],
    user_text: &str,
    redis_conn: Option<Arc<Mutex<MultiplexedConnection>>>,
    topic: &str,
    message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    agent: ChatAgentType,
    queue_invoking_message_id: Option<Uuid>,
) -> Result<GenerateOutcome, BoxError> {
    let client = Gemini::with_model(api_key, gemini_model_id(model))
        .map_err(|e| format!("Gemini client: {}", e))?;

    let system_prompt = system_prompt_for_agent(&agent);
    let declarations: Vec<FunctionDeclaration> = to_gemini_declarations_for_agent(&agent)
        .into_iter()
        .map(sanitize_gemini_function_declaration)
        .collect::<Result<Vec<_>, _>>()?;
    let mut full_text = String::new();
    let mut full_thinking = String::new();
    let mut thinking_signature_capture: Option<String> = None;
    // Incremented only when we publish a non-final assistant text chunk to Redis.
    let mut chunk_index: u32 = 0;
    let mut thinking_chunk_index: u32 = 0;

    let entries = session_messages_to_upstream_entries(session_messages, user_text);
    let mut request = client.generate_content().with_system_prompt(system_prompt);

    for entry in &entries {
        match entry {
            UpstreamHistoryEntry::User { text } => {
                if !text.is_empty() {
                    request = request.with_user_message(text.clone());
                }
            }
            UpstreamHistoryEntry::Assistant {
                text,
                tool_calls,
                thinking_blocks,
            } => {
                if let Some(tbs) = thinking_blocks.as_ref() {
                    for tb in tbs {
                        let has_text = !tb.thinking.is_empty();
                        let has_sig = tb
                            .signature
                            .as_ref()
                            .is_some_and(|s| !s.is_empty());
                        if !has_text && !has_sig {
                            continue;
                        }
                        if has_text && !has_sig {
                            tracing::warn!(
                                "Gemini history: thought block has visible text but empty thoughtSignature; \
                                 API continuity may suffer"
                            );
                        }
                        let model_content = match tb.signature.as_ref().filter(|s| !s.is_empty()) {
                            Some(sig) => {
                                Content::thought_with_signature(tb.thinking.clone(), sig.clone())
                            }
                            None => Content {
                                parts: Some(vec![Part::Text {
                                    text: tb.thinking.clone(),
                                    thought: Some(true),
                                    thought_signature: None,
                                }]),
                                role: None,
                            },
                        }
                        .with_role(Role::Model);
                        request = request.with_message(Message {
                            content: model_content,
                            role: Role::Model,
                        });
                    }
                }
                if let Some(tcs) = tool_calls.as_ref() {
                    if !text.trim().is_empty() {
                        request = request.with_model_message(text.trim().to_string());
                    }
                    for tc in tcs.iter() {
                        if tc.name.is_empty() {
                            continue;
                        }
                        let Some(sig) = &tc.thought_signature else {
                            continue;
                        };
                        let model_content = Content::function_call_with_thought(
                            FunctionCall::new(tc.name.clone(), tc.arguments.clone()),
                            sig.clone(),
                        );
                        request = request.with_message(Message {
                            content: model_content,
                            role: Role::Model,
                        });
                    }
                } else if !text.trim().is_empty() {
                    request = request.with_model_message(text.trim().to_string());
                }
            }
            UpstreamHistoryEntry::ToolResult {
                tool_name,
                content,
                ..
            } => {
                request = request
                    .with_function_response_str(tool_name, content.clone())
                    .map_err(|e| format!("Gemini history function response: {}", e))?;
            }
        }
    }

    request = request.with_function_calling_mode(FunctionCallingMode::Auto);
    for decl in &declarations {
        request = request.with_function(decl.clone());
    }

    if !thinking.explicitly_disabled {
        if let Some(level_str) = thinking.level.as_ref() {
            if thinking.budget.is_some() {
                tracing::warn!(
                    "GEMINI_THINKING_BUDGET is ignored when GEMINI_THINKING_LEVEL is set (Gemini 3 API is mutually exclusive)"
                );
            }
            let level = parse_gemini_thinking_level(level_str)?;
            request = request
                .with_thinking_level(level)
                .with_thoughts_included(true);
        } else if let Some(budget) = thinking.budget {
            request = request
                .with_thinking_budget(budget)
                .with_thoughts_included(budget != 0);
        }
    }

    tracing::info!(
        tool_count = declarations.len(),
        thinking_disabled = thinking.explicitly_disabled,
        thinking_level = ?thinking.level,
        thinking_budget = ?thinking.budget,
        "Gemini: stream started with tools attached"
    );

    let mut stream: GenerationStream = request
        .execute_stream()
        .await
        .map_err(|e| format!("Gemini stream: {}", e))?;

    let mut all_function_calls: Vec<(String, Value, Option<String>)> = Vec::new();
    let mut chunk_count: u32 = 0;

    while let Some(chunk) = futures::TryStreamExt::try_next(&mut stream)
        .await
        .map_err(|e| format!("Gemini stream: {}", e))?
    {
        chunk_count += 1;
        for (fc, thought_sig) in chunk.function_calls_with_thoughts() {
            if fc.name.trim().is_empty() {
                tracing::warn!(
                    chunk_index = chunk_count,
                    "Gemini: skipping streamed function call with empty name (partial chunk?)"
                );
                continue;
            }
            all_function_calls.push((
                fc.name.clone(),
                fc.args.clone(),
                thought_sig.map(String::from),
            ));
            tracing::info!(
                chunk_index = chunk_count,
                tool = %fc.name,
                has_thought_signature = thought_sig.is_some(),
                "Gemini: found function call in stream chunk"
            );
        }
        for (text, is_thought, sig_opt) in chunk.text_with_thoughts() {
            if text.is_empty() {
                continue;
            }
            if is_thought {
                full_thinking.push_str(&text);
                if let Some(s) = sig_opt
                    && !s.is_empty()
                {
                    thinking_signature_capture = Some(s.clone());
                }
                if let Some(ref conn) = redis_conn {
                    let mut guard = conn.lock().await;
                    if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                        return Ok(GenerateOutcome::Stopped(TextPayload {
                            text: full_text,
                            requested_tool_calls: None,
                            thinking_blocks: thinking_blocks_from_gemini_stream(
                                &full_thinking,
                                thinking_signature_capture.clone(),
                            ),
                        }));
                    }
                    let payload = ChatSessionNewThinkingChunkPayload::new(
                        project_id,
                        session_id,
                        message_id,
                        thinking_chunk_index,
                        text.clone(),
                        false,
                    );
                    let event = WsServerEvent::ChatSessionNewThinkingChunk(payload);
                    if let Err(e) = publish_event_to_channel(&mut *guard, topic, &event).await {
                        tracing::warn!("Failed to publish Gemini thinking chunk to Redis: {}", e);
                    }
                    thinking_chunk_index += 1;
                }
            } else {
                full_text.push_str(&text);
                if let Some(ref conn) = redis_conn {
                    let mut guard = conn.lock().await;
                    if is_message_stopped(&mut *guard, message_id).await.ok() == Some(true) {
                        return Ok(GenerateOutcome::Stopped(TextPayload {
                            text: full_text,
                            requested_tool_calls: None,
                            thinking_blocks: thinking_blocks_from_gemini_stream(
                                &full_thinking,
                                thinking_signature_capture.clone(),
                            ),
                        }));
                    }
                    let payload = ChatSessionNewMessageChunkPayload::new(
                        project_id,
                        session_id,
                        message_id,
                        chunk_index,
                        text.clone(),
                        false,
                    );
                    let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
                    if let Err(e) = publish_event_to_channel(&mut *guard, topic, &event).await {
                        tracing::warn!("Failed to publish chat chunk to Redis: {}", e);
                    }
                    chunk_index += 1;
                }
            }
        }
    }

    let fc_count = all_function_calls.len();
    tracing::info!(
        stream_chunks = chunk_count,
        function_call_count = fc_count,
        "Gemini: stream ended, checking for tool calls"
    );

    let had_content_stream = !full_text.is_empty() || !full_thinking.is_empty();
    let thinking_blocks =
        thinking_blocks_from_gemini_stream(&full_thinking, thinking_signature_capture.clone());
    let published_stream_chunks = chunk_index > 0 || thinking_chunk_index > 0;

    let requested_tool_calls = if all_function_calls.is_empty() {
        None
    } else {
        tracing::info!("Gemini: LLM requested {} tool call(s), executing", fc_count);
        let requested: Vec<RequestedToolCall> = all_function_calls
            .iter()
            .map(|(name, args, thought_sig)| RequestedToolCall {
                tool_call_id: Some(new_uuid().to_string()),
                tool_name: name.clone(),
                tool_arguments_to_pass: args.clone(),
                thought_signature: thought_sig.clone(),
            })
            .collect();

        if !had_content_stream {
            let invoker_payload = TextPayload {
                text: full_text.clone(),
                requested_tool_calls: Some(requested.clone()),
                thinking_blocks: thinking_blocks.clone(),
            };
            if let Some(ref conn) = redis_conn {
                ai_message::create_ai_message_with_payload_and_publish(
                    pool,
                    conn,
                    session_id,
                    message_id,
                    user_id,
                    project_id,
                    &invoker_payload,
                    agent.clone(),
                    queue_invoking_message_id,
                )
                .await
                .map_err(|e| format!("ai_message: {}", e))?;
            }
        }

        for (req, (name, args, _thought_sig)) in requested.iter().zip(all_function_calls.iter()) {
            tracing::info!(tool = %name, args = ?args, "Gemini: executing tool");
            tools::execute_tool_async(name, args.clone(), req.tool_call_id.clone())
                .await
                .map_err(|e| format!("Tool execution failed: {}", e))?;
        }
        Some(requested)
    };

    if all_function_calls.is_empty() {
        tracing::info!("Gemini: no tool calls in any stream chunk (function_call_count=0)");
    }

    if published_stream_chunks && let Some(ref conn) = redis_conn {
        let mut guard = conn.lock().await;
        if thinking_chunk_index > 0 {
            let thinking_done = ChatSessionNewThinkingChunkPayload::new(
                project_id,
                session_id,
                message_id,
                thinking_chunk_index,
                String::new(),
                true,
            );
            if let Err(e) = publish_event_to_channel(
                &mut *guard,
                topic,
                &WsServerEvent::ChatSessionNewThinkingChunk(thinking_done),
            )
            .await
            {
                tracing::warn!("Failed to publish final Gemini thinking chunk to Redis: {}", e);
            }
        }
        let payload = ChatSessionNewMessageChunkPayload::new(
            project_id,
            session_id,
            message_id,
            chunk_index,
            String::new(),
            true,
        );
        let event = WsServerEvent::ChatSessionNewMessageChunk(payload);
        if let Err(e) = publish_event_to_channel(&mut *guard, topic, &event).await {
            tracing::warn!("Failed to publish final chat chunk to Redis: {}", e);
        }
    }

    Ok(GenerateOutcome::Completed(TextPayload {
        text: full_text,
        requested_tool_calls,
        thinking_blocks,
    }))
}
