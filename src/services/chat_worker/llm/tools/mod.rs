//! # Tool module standard (one tool per file)
//!
//! Every tool module must provide:
//!
//! - **`TOOL_NAME: &'static str`** — Unique tool name for the LLM.
//! - **`MESSAGE_TYPE: ChatMessageType`** — Message type for queue/DB messages produced by this tool.
//! - **`spec() -> ToolSpec`** — Name, description, and JSON schema for parameters (for LLM declarations).
//! - **`handler(...)`** — Async fn that processes a consumed queue message (same signature for all tools: DB pool, Redis, ids, typed [`ChatMessagePayload`], optional Rabbit publisher).
//! - **`payload_from_args(args: Value) -> Result<ChatMessagePayload, BoxError>`** — Builds queue payload from LLM args (used by [`tool_context_for_chat_session`]). Use only for JSON shape / coercion; **normalize** simple LLM mistakes here when safe (e.g. trim, ASCII lowercase for slug and enum-slug fields, full `unit_id` strings). **business validation** (invalid ids, empty required fields, etc.) belongs in **`handler`**, which should complete the message with `status` **Completed** and `error` set on the payload—never `return Err` from `payload_from_args` for that, or the whole turn fails before the tool row is queued. **Serde / schema validation** `Err` from `payload_from_args` is persisted as a **Failed** tool row (with `error`, `raw_arguments`, `tool_call_id`, `tool_name`) so the next LLM turn can see it in history.
//! - **`gemini_declaration() -> FunctionDeclaration`** — Gemini declaration with parameter schema (used in `llm/gemini.rs`).
//! - **Parameter descriptions** — For each argument field, define a `pub const ARG_<NAME>_DESCRIPTION: &str` and use it in both `#[schemars(description = ARG_<NAME>_DESCRIPTION)]` on the args struct and in `params_schema()` (OpenAI/Claude) so Gemini and other providers stay aligned.
//!
//! To add a new tool: add `mod my_tool;`, add `my_tool::spec()` to [`all_tool_specs`],
//! add `(my_tool::MESSAGE_TYPE, my_tool::handler)` to [`tool_message_handlers`],
//! add `(my_tool::TOOL_NAME, my_tool::MESSAGE_TYPE, my_tool::payload_from_args)` to [`tool_queue_registry`],
//! and add `(my_tool::TOOL_NAME, my_tool::gemini_declaration)` in `gemini_declaration_fns_by_name` in `llm/gemini.rs`.
//!
//! Agent-style and finish-style tools listed in [`tool_message_type_skips_generic_invoker_completion`]
//! do not run [`on_tool_message_finished`] on success (custom lifecycle). On handler error they still
//! mark the tool row failed and run [`maybe_complete_invoking_message`] so the parent AI message can unblock.

pub mod analyze_architecture_tool;
pub mod analyze_solution_tool;
pub mod create_unit;
pub mod finish_architecture_analysis;
pub mod finish_implementation;
pub mod finish_solution_analysis;
pub mod implement_solution_tool;
pub mod list_modules_for_unit_contexts;
pub mod list_unit_contexts;
pub mod load_latest_production_release_units;
pub mod load_latest_releases;
pub mod modify_unit;
pub mod remove_unit;

/// Re-exported for use in [`crate::models::chat_messages::ChatMessagePayload`].
pub use analyze_architecture_tool::ToolAnalyzeArchitecturePayload;
pub use analyze_solution_tool::ToolAnalyzeSolutionPayload;
pub use create_unit::ToolCreateUnitPayload;
pub use finish_architecture_analysis::ToolFinishArchitectureAnalysisPayload;
pub use finish_implementation::ToolFinishImplementationPayload;
pub use finish_solution_analysis::ToolFinishSolutionAnalysisPayload;
pub use implement_solution_tool::ToolImplementSolutionPayload;
pub use list_modules_for_unit_contexts::ToolListModulesForUnitContextsPayload;
pub use list_unit_contexts::ToolListUnitContextsPayload;
pub use load_latest_production_release_units::ToolFetchRelevantUnitsPayload;
pub use load_latest_releases::ToolFetchLatestReleasesPayload;
pub use modify_unit::ToolModifyUnitPayload;
pub use remove_unit::ToolRemoveUnitPayload;

/// Status of an internal sub-task check attached to unit tool messages.
/// Stored in DB payload for client UI; **never sent to the LLM** in history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitSubTaskStatus {
    Succeeded,
    Failed,
}

use crate::database::{DbPool, block_on_db_tokio};
use crate::libs::ids::new_uuid;
use crate::models::chat_messages::{
    ChatAgentType, ChatMessagePayload, ChatMessageResponse, ChatMessageStatus,
    ChatMessageType, ChatSessionRoleType, TextPayload,
};
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::routes::ws::events::{
    ChatSessionMessageCreatedPayload, ChatSessionMessageStatusChangePayload, WsServerEvent,
};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use uuid::Uuid;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Callback type for queue-based tools: invoked with (args, tool_call_id) and returns the new message id.
pub type OnToolCall = Arc<
    dyn Fn(Value, Option<String>) -> Pin<Box<dyn Future<Output = Result<Uuid, BoxError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// Context for tool execution (callbacks keyed by TOOL_NAME). Set once per request via [`with_tool_context`].
#[derive(Clone)]
pub struct ToolContext {
    /// Map from tool name (e.g. each module’s `TOOL_NAME`) to callback that publishes to the queue.
    pub callbacks: HashMap<String, OnToolCall>,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            callbacks: HashMap::new(),
        }
    }
}

tokio::task_local! {
    /// Task-local tool context so the LLM layer does not need to pass callbacks through.
    static TOOL_CONTEXT: Arc<ToolContext>;
}

/// Runs the given future with the tool context set. Call this at the entry point (e.g. processor)
/// so that [`execute_tool_async`] can resolve async tools without any callback parameters.
pub async fn with_tool_context<F, R>(ctx: ToolContext, f: F) -> R
where
    F: Future<Output = R>,
{
    TOOL_CONTEXT.scope(Arc::new(ctx), f).await
}

/// Function type each queue-based tool implements: (args, tool_call_id, tool_name) from LLM → payload for the queue message.
type PayloadFromArgsFn = fn(Value, Option<String>, &str) -> Result<ChatMessagePayload, BoxError>;

/// Registry of (TOOL_NAME, MESSAGE_TYPE, payload_from_args). Add new tools here.
fn tool_queue_registry() -> &'static [(&'static str, ChatMessageType, PayloadFromArgsFn)] {
    &[
        (
            load_latest_production_release_units::TOOL_NAME,
            load_latest_production_release_units::MESSAGE_TYPE,
            load_latest_production_release_units::payload_from_args,
        ),
        (
            load_latest_releases::TOOL_NAME,
            load_latest_releases::MESSAGE_TYPE,
            load_latest_releases::payload_from_args,
        ),
        (
            list_unit_contexts::TOOL_NAME,
            list_unit_contexts::MESSAGE_TYPE,
            list_unit_contexts::payload_from_args,
        ),
        (
            list_modules_for_unit_contexts::TOOL_NAME,
            list_modules_for_unit_contexts::MESSAGE_TYPE,
            list_modules_for_unit_contexts::payload_from_args,
        ),
        (
            analyze_architecture_tool::TOOL_NAME,
            analyze_architecture_tool::MESSAGE_TYPE,
            analyze_architecture_tool::payload_from_args,
        ),
        (
            finish_architecture_analysis::TOOL_NAME,
            finish_architecture_analysis::MESSAGE_TYPE,
            finish_architecture_analysis::payload_from_args,
        ),
        (
            analyze_solution_tool::TOOL_NAME,
            analyze_solution_tool::MESSAGE_TYPE,
            analyze_solution_tool::payload_from_args,
        ),
        (
            finish_solution_analysis::TOOL_NAME,
            finish_solution_analysis::MESSAGE_TYPE,
            finish_solution_analysis::payload_from_args,
        ),
        (
            implement_solution_tool::TOOL_NAME,
            implement_solution_tool::MESSAGE_TYPE,
            implement_solution_tool::payload_from_args,
        ),
        (
            finish_implementation::TOOL_NAME,
            finish_implementation::MESSAGE_TYPE,
            finish_implementation::payload_from_args,
        ),
        (
            create_unit::TOOL_NAME,
            create_unit::MESSAGE_TYPE,
            create_unit::payload_from_args,
        ),
        (
            modify_unit::TOOL_NAME,
            modify_unit::MESSAGE_TYPE,
            modify_unit::payload_from_args,
        ),
        (
            remove_unit::TOOL_NAME,
            remove_unit::MESSAGE_TYPE,
            remove_unit::payload_from_args,
        ),
    ]
}

pub use crate::services::chat_worker::agent::tool_names_for_llm_session;

fn tool_queue_registry_for_agent(
    agent: &ChatAgentType,
) -> Vec<(&'static str, ChatMessageType, PayloadFromArgsFn)> {
    let names = tool_names_for_llm_session(agent);
    tool_queue_registry()
        .iter()
        .filter(|(name, _, _)| names.contains(name))
        .map(|(name, msg_type, fn_ptr)| (*name, *msg_type, *fn_ptr))
        .collect()
}

/// Builds a [`ToolContext`] with one callback per tool for this agent (see [`tool_names_for_llm_session`]). Each callback
/// converts args to a queue payload and publishes. Use at the entry point (e.g. processor) before
/// calling [`with_tool_context`].
///
/// **`invoking_message_id`** here is the id of the **current AI `Text` message** being generated in
/// this `process_one` pass (the row that will hold `requested_tool_calls`). Each queued **tool** row
/// stores that same value in its DB column `invoking_message_id` — meaning “this tool was requested
/// by that AI Text message” (not the session root, not the tool’s own id). [`maybe_complete_invoking_message`]
/// is later called with **that AI Text message’s id** to complete the turn once all those tool rows finish.
///
/// Special case: async tools like [`analyze_architecture_tool`], [`analyze_solution_tool`], and [`implement_solution_tool`] publish a **separate** queue
/// message for a sub-agent run; that message sets `invoking_message_id` to the **tool row** id so
/// the sub-agent’s `Text` output is nested under the tool (see `ChatSessionQueueMessage` docs).
/// `agent` is stored on the queue message so the worker can set it on the created message row.
pub fn tool_context_for_chat_session(
    pool: DbPool,
    redis_conn: RedisConn,
    publisher: Arc<SharedRabbitPublisher>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    invoking_message_id: Uuid,
    agent: ChatAgentType,
) -> ToolContext {
    let mut callbacks = HashMap::new();
    for (name, message_type, payload_from_args) in tool_queue_registry_for_agent(&agent)
    {
        let pool = pool.clone();
        let redis_conn = redis_conn.clone();
        let publisher = publisher.clone();
        let agent = agent.clone();
        // Own the name per callback so `payload_from_args` always receives this tool’s name (same as the map key).
        let registered_tool_name = (*name).to_string();
        callbacks.insert(
            registered_tool_name.clone(),
            Arc::new(move |args: Value, tool_call_id: Option<String>| {
                let pool = pool.clone();
                let redis_conn = redis_conn.clone();
                let publisher = publisher.clone();
                let user_id = user_id;
                let project_id = project_id;
                let session_id = session_id;
                let message_type = message_type;
                let payload_from_args = payload_from_args;
                let agent = agent.clone();
                let registered_tool_name = registered_tool_name.clone();
                Box::pin(async move {
                    let response_message_id = new_uuid();
                    let args_for_failure = args.clone();
                    let tool_call_id_for_failure = tool_call_id.clone();
                    let payload = match payload_from_args(
                        args,
                        tool_call_id.clone(),
                        registered_tool_name.as_str(),
                    ) {
                        Ok(p) => p,
                        Err(e) => {
                            persist_tool_args_parse_failure(
                                &pool,
                                &redis_conn,
                                user_id,
                                project_id,
                                session_id,
                                invoking_message_id,
                                agent.clone(),
                                message_type,
                                response_message_id,
                                registered_tool_name.as_str(),
                                tool_call_id_for_failure,
                                &args_for_failure,
                                &e.to_string(),
                            )
                            .await;
                            return Err(e);
                        }
                    };
                    let payload_json = serde_json::to_value(&payload)
                        .map_err(|e| -> BoxError { format!("queue tool payload json: {}", e).into() })?;
                    let message = ChatSessionQueueMessage {
                        user_id,
                        role: ChatSessionRoleType::Ai,
                        type_: message_type,
                        payload: Some(payload_json),
                        response_message_id,
                        retry_num: 0,
                        invoking_message_id: Some(invoking_message_id),
                        agent,
                        sub_agent_stuck_continuation_count: 0,
                    };
                    chat_message_queue::publish_with_publisher(
                        &publisher, project_id, session_id, &message,
                    )
                    .await
                    .map_err(|e| -> BoxError { Box::new(e) })?;
                    Ok(response_message_id)
                }) as Pin<Box<dyn Future<Output = Result<Uuid, BoxError>> + Send>>
            }) as OnToolCall,
        );
    }
    ToolContext { callbacks }
}

/// Inserts a Failed tool row when `payload_from_args` rejects LLM arguments (nothing is published to
/// the queue). The next LLM pass loads this row in session history so the model can correct inputs.
#[allow(clippy::too_many_arguments)]
async fn persist_tool_args_parse_failure(
    pool: &DbPool,
    redis_conn: &RedisConn,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    invoking_message_id: Uuid,
    agent: ChatAgentType,
    message_type: ChatMessageType,
    message_id: Uuid,
    tool_name: &str,
    tool_call_id: Option<String>,
    raw_arguments: &Value,
    error: &str,
) {
    let payload = serde_json::json!({
        "error": error,
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "raw_arguments": raw_arguments,
    });
    let insert_message_id = message_id;
    let session_id_ins = session_id;
    let agent_ins = agent.clone();
    let row = match block_on_db_tokio(pool, move |conn| {
        diesel::insert_into(chat_messages::table)
            .values((
                chat_messages::id.eq(insert_message_id),
                chat_messages::chat_session_id.eq(session_id_ins),
                chat_messages::role.eq(ChatSessionRoleType::Ai),
                chat_messages::type_.eq(message_type),
                chat_messages::payload.eq(Some(payload.clone())),
                chat_messages::status.eq(ChatMessageStatus::Failed),
                chat_messages::invoking_message_id.eq(Some(invoking_message_id)),
                chat_messages::agent.eq(Some(agent_ins)),
            ))
            .returning(crate::models::chat_messages::ChatMessageRow::as_returning())
            .get_result(conn)
    })
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(
                %message_id,
                %tool_name,
                "persist_tool_args_parse_failure: insert failed: {}",
                e
            );
            return;
        }
    };
    let channel = user_channel(user_id);
    let created_message = ChatMessageResponse::from(row);
    let event = WsServerEvent::ChatSessionMessageCreated(ChatSessionMessageCreatedPayload {
        project_id,
        session_id,
        message: created_message,
    });
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
        tracing::warn!(
            "persist_tool_args_parse_failure: failed to publish ChatSessionMessageCreated: {}",
            e
        );
    }
}

pub fn all_tool_specs() -> Vec<ToolSpec> {
    vec![
        load_latest_production_release_units::spec(),
        load_latest_releases::spec(),
        list_unit_contexts::spec(),
        list_modules_for_unit_contexts::spec(),
        analyze_architecture_tool::spec(),
        finish_architecture_analysis::spec(),
        analyze_solution_tool::spec(),
        finish_solution_analysis::spec(),
        implement_solution_tool::spec(),
        finish_implementation::spec(),
        create_unit::spec(),
        modify_unit::spec(),
        remove_unit::spec(),
    ]
}

/// Single entry point for the LLM layer: runs queue-based tools via the callback registered under `name`.
/// All tools publish to RabbitMQ; worker creates message and spawns handler.
/// Pass `tool_call_id` when the provider supplies it (e.g. Claude, OpenAI) so tool results can be attached correctly in history.
pub async fn execute_tool_async(
    name: &str,
    args: Value,
    tool_call_id: Option<String>,
) -> Result<String, BoxError> {
    let cb = TOOL_CONTEXT
        .try_with(|ctx| ctx.callbacks.get(name).cloned())
        .map_err(|_| "tools: TOOL_CONTEXT not set (call with_tool_context first)".to_string())?
        .ok_or_else(|| format!("Unknown tool or callback not registered: {}", name))?;
    cb(args, tool_call_id).await?;
    Ok("Request queued.".to_string())
}

// ----- Tool message handlers (runner → worker) -----

type RedisConn = Arc<Mutex<redis::aio::MultiplexedConnection>>;

/// Persists a tool message as Failed in the DB, merges `error` into the message JSON payload when
/// possible (so LLM history replays include the failure reason), and notifies the client via WebSocket.
/// Call this from tool handlers on any failure path so that (1) the tool row shows Failed when
/// listing messages from DB, and (2) maybe_complete_invoking_message can see it and update the
/// invoker to Failed in DB and over WS.
pub async fn set_tool_message_failed(
    pool: &DbPool,
    redis_conn: &RedisConn,
    message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    error: String,
) {
    let error_for_db = error.clone();
    if let Err(e) = block_on_db_tokio(pool, move |conn| {
        let row_opt = chat_messages::table
            .filter(chat_messages::id.eq(message_id))
            .select(chat_messages::payload)
            .first::<Option<serde_json::Value>>(conn)
            .optional()?;

        let Some(maybe_payload) = row_opt else {
            return Ok(());
        };

        let new_payload = match maybe_payload {
            Some(mut v) if v.is_object() => {
                if let Value::Object(ref mut map) = v {
                    map.insert(
                        "error".to_string(),
                        Value::String(error_for_db.clone()),
                    );
                }
                v
            }
            Some(_) | None => serde_json::json!({ "error": error_for_db }),
        };

        diesel::update(chat_messages::table.filter(chat_messages::id.eq(message_id)))
            .set((
                chat_messages::status.eq(ChatMessageStatus::Failed),
                chat_messages::payload.eq(Some(new_payload)),
            ))
            .execute(conn)?;
        Ok::<_, diesel::result::Error>(())
    })
    .await
    {
        tracing::error!(%message_id, "set_tool_message_failed: DB update failed: {}", e);
    }
    let channel = user_channel(user_id);
    let event =
        WsServerEvent::ChatSessionMessageStatusChange(ChatSessionMessageStatusChangePayload {
            project_id,
            session_id,
            message_id,
            status: ChatMessageStatus::Failed,
        });
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
        tracing::warn!(
            "set_tool_message_failed: failed to publish status change: {}",
            e
        );
    }
}

/// Arguments passed to a tool message handler when a queue message is consumed.
/// Handlers receive this struct so the meaning of each value is clear.
#[derive(Clone)]
pub struct ToolMessageContext {
    pub pool: DbPool,
    pub redis_conn: RedisConn,
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub user_id: Uuid,
    pub message_type: ChatMessageType,
    /// Tool message payload from the queue (same logical content as stored on the message).
    /// Passed as the typed enum so handlers do not depend on a lossy JSON round-trip.
    pub payload: Option<ChatMessagePayload>,
    /// Id of the **AI `Text` message** that listed this tool in `requested_tool_calls` (same value as the
    /// tool row’s DB column `invoking_message_id`). This is **not** [`Self::message_id`] (the tool row),
    /// and **not** the session root unless the invoker happens to be the root planner turn. When all
    /// tool siblings sharing this invoker are done, that AI message is completed and a queue continuation may run.
    pub invoking_message_id: Option<Uuid>,
    /// When set, after all tool messages for the invoking message are done we publish a continuation message to RabbitMQ so the LLM continues generating (same as POST flow).
    pub rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
}

/// Type of a handler that processes a tool message (e.g. after a queue message is consumed).
/// Handlers return `Ok(())` on success; on `Err`, the caller marks the message Failed and runs invoker completion.
pub type ToolMessageHandler = Arc<
    dyn Fn(ToolMessageContext) -> Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send>>
        + Send
        + Sync,
>;

/// Wraps an async handler that takes payload (and optional Rabbit publisher) into a [`ToolMessageHandler`].
fn wrap_handler_with_payload<F, Fut>(handler: F) -> ToolMessageHandler
where
    F: Fn(
        DbPool,
        RedisConn,
        Uuid,
        Uuid,
        Uuid,
        Uuid,
        Option<ChatMessagePayload>,
        Option<Arc<SharedRabbitPublisher>>,
    ) -> Fut
        + Send
        + Sync
        + 'static,
    Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
{
    Arc::new(move |ctx: ToolMessageContext| {
        Box::pin(handler(
            ctx.pool,
            ctx.redis_conn,
            ctx.project_id,
            ctx.session_id,
            ctx.message_id,
            ctx.user_id,
            ctx.payload,
            ctx.rabbit_publisher.clone(),
        ))
    })
}

/// Tool types that manage their own invoker / parent completion (or defer it to another tool).
/// All other tool types always go through [`on_tool_message_finished`] after the handler returns.
pub fn tool_message_type_skips_generic_invoker_completion(type_: &ChatMessageType) -> bool {
    matches!(
        type_,
        ChatMessageType::ToolAnalyzeArchitecture | ChatMessageType::ToolFinishArchitectureAnalysis
            | ChatMessageType::ToolAnalyzeSolution
            | ChatMessageType::ToolFinishSolutionAnalysis
            | ChatMessageType::ToolImplementSolution
            | ChatMessageType::ToolFinishImplementation
    )
}

/// Called after a tool handler returns. If the handler failed, persists Failed (DB + WS) for the tool message;
/// then runs [`maybe_complete_invoking_message`] so the invoker is marked Completed or Failed when all its tools are done.
pub async fn on_tool_message_finished(ctx: &ToolMessageContext, result: Result<(), BoxError>) {
    if let Err(e) = &result {
        tracing::error!(%ctx.message_id, "Tool handler failed: {}", e);
        set_tool_message_failed(
            &ctx.pool,
            &ctx.redis_conn,
            ctx.message_id,
            ctx.user_id,
            ctx.project_id,
            ctx.session_id,
            e.to_string(),
        )
        .await;
    }
    if let Some(invoking_message_id) = ctx.invoking_message_id {
        maybe_complete_invoking_message(
            &ctx.pool,
            &ctx.redis_conn,
            ctx.rabbit_publisher.as_ref(),
            invoking_message_id,
            ctx.user_id,
            ctx.project_id,
            ctx.session_id,
            true,
        )
        .await;
    }
}

/// List of (MESSAGE_TYPE, handler) for all queue-based tools. Add new tools here.
fn tool_message_handlers() -> &'static [(ChatMessageType, ToolMessageHandler)] {
    static HANDLERS: OnceLock<Vec<(ChatMessageType, ToolMessageHandler)>> = OnceLock::new();
    HANDLERS
        .get_or_init(|| {
            vec![
                (
                    load_latest_production_release_units::MESSAGE_TYPE,
                    wrap_handler_with_payload(load_latest_production_release_units::handler),
                ),
                (
                    load_latest_releases::MESSAGE_TYPE,
                    wrap_handler_with_payload(load_latest_releases::handler),
                ),
                (
                    list_unit_contexts::MESSAGE_TYPE,
                    wrap_handler_with_payload(list_unit_contexts::handler),
                ),
                (
                    list_modules_for_unit_contexts::MESSAGE_TYPE,
                    wrap_handler_with_payload(list_modules_for_unit_contexts::handler),
                ),
                (
                    analyze_architecture_tool::MESSAGE_TYPE,
                    wrap_handler_with_payload(analyze_architecture_tool::handler),
                ),
                (
                    finish_architecture_analysis::MESSAGE_TYPE,
                    wrap_handler_with_payload(finish_architecture_analysis::handler),
                ),
                (
                    analyze_solution_tool::MESSAGE_TYPE,
                    wrap_handler_with_payload(analyze_solution_tool::handler),
                ),
                (
                    finish_solution_analysis::MESSAGE_TYPE,
                    wrap_handler_with_payload(finish_solution_analysis::handler),
                ),
                (
                    implement_solution_tool::MESSAGE_TYPE,
                    wrap_handler_with_payload(implement_solution_tool::handler),
                ),
                (
                    finish_implementation::MESSAGE_TYPE,
                    wrap_handler_with_payload(finish_implementation::handler),
                ),
                (
                    create_unit::MESSAGE_TYPE,
                    wrap_handler_with_payload(create_unit::handler),
                ),
                (
                    modify_unit::MESSAGE_TYPE,
                    wrap_handler_with_payload(modify_unit::handler),
                ),
                (
                    remove_unit::MESSAGE_TYPE,
                    wrap_handler_with_payload(remove_unit::handler),
                ),
            ]
        })
        .as_slice()
}

/// Returns the handler for the given message type, if any.
fn tool_message_handler(type_: ChatMessageType) -> Option<ToolMessageHandler> {
    tool_message_handlers()
        .iter()
        .find(|(t, _)| t == &type_)
        .map(|(_, h)| h.clone())
}

/// After a tool message completes (success or failure), check whether the **AI `Text` message that
/// triggered those tools** is ready to finish.
///
/// **`invoking_message_id`** must be the **id of that AI `Text` row** (the one that will be set
/// to Completed/Failed), i.e. the same id stored on each tool row as the column `invoking_message_id`.
/// It is **not** a tool message id, and **not** “the root ancestor” of the session — it is exactly the
/// message that listed `requested_tool_calls`. Nested sub-agent `Text` rows that hang under a **tool**
/// row are completed via a separate call with **that** sub-agent AI message’s id.
///
/// Direct tool children are rows whose **column** `invoking_message_id` equals `invoking_message_id`.
///
/// If all such children are done, update that AI message and optionally publish a continuation whose
/// `agent` and `invoking_message_id` are copied from **that invoker row** (its column `invoking_message_id`
/// is where the turn hangs in the tree — `NULL` for main-thread planner, or the outer tool row for nested agents).
///
/// When `publish_queue_continuation` is `false`, the invoker row is still updated and WS status is sent,
/// but no RabbitMQ continuation is published (used when completing a nested sub-agent `Text` row so the
/// worker does not schedule another pass for that sub-agent).
pub async fn maybe_complete_invoking_message(
    pool: &DbPool,
    redis_conn: &RedisConn,
    rabbit_publisher: Option<&Arc<SharedRabbitPublisher>>,
    invoking_message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    publish_queue_continuation: bool,
) {
    let (total, done, failed_count): (i64, i64, i64) = match block_on_db_tokio(pool, move |conn| {
        let total: i64 = chat_messages::table
            .filter(chat_messages::invoking_message_id.eq(invoking_message_id))
            .count()
            .get_result(conn)?;
        let done: i64 = chat_messages::table
            .filter(chat_messages::invoking_message_id.eq(invoking_message_id))
            .filter(
                chat_messages::status
                    .eq(ChatMessageStatus::Completed)
                    .or(chat_messages::status.eq(ChatMessageStatus::Failed)),
            )
            .count()
            .get_result(conn)?;
        let failed_count: i64 = chat_messages::table
            .filter(chat_messages::invoking_message_id.eq(invoking_message_id))
            .filter(chat_messages::status.eq(ChatMessageStatus::Failed))
            .count()
            .get_result(conn)?;
        Ok::<_, diesel::result::Error>((total, done, failed_count))
    })
    .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(
                %invoking_message_id,
                "maybe_complete_invoking_message: DB error: {}",
                e
            );
            return;
        }
    };
    if total == 0 || done != total {
        return;
    }
    // If any tool failed, parent must be Failed. Only set Completed when none failed, and never overwrite an existing Failed (tools complete in arbitrary order).
    let new_status = if failed_count > 0 {
        ChatMessageStatus::Failed
    } else {
        ChatMessageStatus::Completed
    };
    let new_status_for_db = new_status.clone();
    let is_failed = failed_count > 0;
    let rows_updated = match block_on_db_tokio(pool, move |conn| {
        let base = diesel::update(
            chat_messages::table.filter(chat_messages::id.eq(invoking_message_id)),
        );
        let rows = if is_failed {
            base.set(chat_messages::status.eq(new_status_for_db))
                .execute(conn)?
        } else {
            // Only set Completed when parent is still Generating; do not override Failed.
            base.set(chat_messages::status.eq(new_status_for_db))
                .filter(chat_messages::status.eq(ChatMessageStatus::Generating))
                .execute(conn)?
        };
        Ok::<_, diesel::result::Error>(rows)
    })
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(
                %invoking_message_id,
                "maybe_complete_invoking_message: failed to update parent: {}",
                e
            );
            return;
        }
    };
    if rows_updated == 0 {
        return; // Another completion already set Failed (or we raced with Completed); don't overwrite or re-publish.
    }
    let channel = user_channel(user_id);
    let event =
        WsServerEvent::ChatSessionMessageStatusChange(ChatSessionMessageStatusChangePayload {
            project_id,
            session_id,
            message_id: invoking_message_id,
            status: new_status,
        });
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
        tracing::warn!(
            "maybe_complete_invoking_message: failed to publish status change: {}",
            e
        );
    }
    drop(guard);

    // Publish to RabbitMQ so the worker runs process_one again and the LLM continues.
    // Read **the invoker AI Text row** (`invoking_message_id`): its `agent` and **column**
    // `invoking_message_id` (thread parent — main thread vs nested under a tool) go on the queue message.
    let (queue_agent, queue_invoking_message_id) =
        match block_on_db_tokio(pool, move |conn| {
            chat_messages::table
                .filter(chat_messages::id.eq(invoking_message_id))
                .select((chat_messages::agent, chat_messages::invoking_message_id))
                .first::<(Option<ChatAgentType>, Option<Uuid>)>(conn)
                .optional()
        })
        .await
        {
            Ok(Some((agent, inv))) => (agent.unwrap_or(ChatAgentType::Planner), inv),
            Ok(None) => {
                tracing::warn!(
                    %invoking_message_id,
                    "maybe_complete_invoking_message: invoker row missing for queue routing; using Planner + no invoking id"
                );
                (ChatAgentType::Planner, None)
            }
            Err(e) => {
                tracing::error!(
                    %invoking_message_id,
                    "maybe_complete_invoking_message: failed to load invoker row for queue routing: {}; using Planner + no invoking id",
                    e
                );
                (ChatAgentType::Planner, None)
            }
        };

    if publish_queue_continuation
        && let Some(publisher) = rabbit_publisher {
        let response_message_id = new_uuid();
        let continuation_payload = serde_json::to_value(ChatMessagePayload::Text(TextPayload {
            text: String::new(),
            ..Default::default()
        }))
        .expect("serialize empty TextPayload for queue continuation");
        let queue_message = ChatSessionQueueMessage {
            user_id,
            role: ChatSessionRoleType::Ai,
            type_: ChatMessageType::Text,
            payload: Some(continuation_payload),
            response_message_id,
            retry_num: 0,
            invoking_message_id: queue_invoking_message_id,
            agent: queue_agent.clone(),
            sub_agent_stuck_continuation_count: 0,
        };
        if let Err(e) = chat_message_queue::publish_with_publisher(
            publisher,
            project_id,
            session_id,
            &queue_message,
        )
        .await
        {
            tracing::error!(
                %invoking_message_id,
                %project_id,
                %session_id,
                "maybe_complete_invoking_message: failed to publish continuation to queue: {}",
                e
            );
        } else {
            tracing::info!(
                %invoking_message_id,
                %response_message_id,
                ?queue_agent,
                ?queue_invoking_message_id,
                "Published message to queue so LLM continues after tool calls"
            );
        }
    }
}

/// When an LLM pass for a nested sub-agent fails (retries exhausted, dead letter, etc.) before the
/// corresponding `finish_*` tool runs, the Planner-facing async tool row (the sub-agent thread’s
/// parent row) stays `Generating`. That prevents [`maybe_complete_invoking_message`] on the Planner
/// from ever running, so the chat stays in a generating state. This marks that parent tool row
/// failed (same shape as handler failures) and completes the Planner invoker with a queue
/// continuation so the Planner can react.
/// Returns `true` when a Planner queue continuation was requested (parent tool marked failed and
/// [`maybe_complete_invoking_message`] ran with `publish_queue_continuation: true`). Caller should
/// treat that like `will_schedule_continuation` and avoid broadcasting session generation ended.
pub async fn cascade_sub_agent_ai_text_failure_to_planner(
    pool: &DbPool,
    redis_conn: &RedisConn,
    rabbit_publisher: Option<&Arc<SharedRabbitPublisher>>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    failed_ai_text_message_id: Uuid,
    agent: ChatAgentType,
    error: String,
) -> bool {
    if matches!(agent, ChatAgentType::Planner) {
        return false;
    }

    let Some(parent_tool_id) = (match block_on_db_tokio(pool, move |conn| {
        let row = chat_messages::table
            .filter(chat_messages::id.eq(failed_ai_text_message_id))
            .filter(chat_messages::chat_session_id.eq(session_id))
            .select((chat_messages::invoking_message_id, chat_messages::type_))
            .first::<(Option<Uuid>, ChatMessageType)>(conn)
            .optional()?;
        Ok::<_, diesel::result::Error>(row)
    })
    .await
    {
        Ok(Some((Some(tid), ChatMessageType::Text))) => Some(tid),
        Ok(Some((None, ChatMessageType::Text))) | Ok(None) => None,
        Ok(Some((_, t))) => {
            tracing::warn!(
                %failed_ai_text_message_id,
                ?t,
                "cascade_sub_agent_ai_text_failure: expected Text row; skipping"
            );
            None
        }
        Err(e) => {
            tracing::error!(
                %failed_ai_text_message_id,
                "cascade_sub_agent_ai_text_failure: load AI row: {}",
                e
            );
            None
        }
    }) else {
        return false;
    };

    let Some(planner_invoker_id) = (match block_on_db_tokio(pool, move |conn| {
        let tool_row = chat_messages::table
            .filter(chat_messages::id.eq(parent_tool_id))
            .filter(chat_messages::chat_session_id.eq(session_id))
            .select((chat_messages::type_, chat_messages::invoking_message_id))
            .first::<(ChatMessageType, Option<Uuid>)>(conn)
            .optional()?;
        Ok::<_, diesel::result::Error>(tool_row)
    })
    .await
    {
        Ok(Some((ChatMessageType::Text, _))) => {
            tracing::warn!(
                %parent_tool_id,
                "cascade_sub_agent_ai_text_failure: expected async tool row as parent; skipping"
            );
            None
        }
        Ok(Some((_, Some(pid)))) => Some(pid),
        Ok(Some((_, None))) => {
            tracing::warn!(
                %parent_tool_id,
                "cascade_sub_agent_ai_text_failure: parent tool row has no planner invoker; skipping"
            );
            None
        }
        Ok(None) => {
            tracing::warn!(
                %parent_tool_id,
                "cascade_sub_agent_ai_text_failure: parent tool row missing; skipping"
            );
            None
        }
        Err(e) => {
            tracing::error!(
                %parent_tool_id,
                "cascade_sub_agent_ai_text_failure: load tool row: {}",
                e
            );
            None
        }
    }) else {
        return false;
    };

    set_tool_message_failed(
        pool,
        redis_conn,
        parent_tool_id,
        user_id,
        project_id,
        session_id,
        error,
    )
    .await;

    maybe_complete_invoking_message(
        pool,
        redis_conn,
        rabbit_publisher,
        planner_invoker_id,
        user_id,
        project_id,
        session_id,
        true,
    )
    .await;
    true
}

/// Spawns the appropriate tool handler for the given message type. Does nothing if the type
/// has no registered handler. Call this from the runner instead of matching on each tool type.
/// `payload` is the tool message payload from the queue (same variant as `message_type`).
pub fn spawn_tool_message_handler(type_: ChatMessageType, ctx: ToolMessageContext) -> bool {
    if let Some(handler) = tool_message_handler(type_) {
        tokio::spawn(async move {
            let result = handler(ctx.clone()).await;
            if tool_message_type_skips_generic_invoker_completion(&ctx.message_type) {
                match &result {
                    Ok(()) => return,
                    Err(e) => {
                        tracing::error!(
                            %ctx.message_id,
                            ?ctx.message_type,
                            "Tool handler failed (agent/finish tool): {}",
                            e
                        );
                        on_tool_message_finished(
                            &ctx,
                            Err(e.to_string().into()),
                        )
                        .await;
                    }
                }
                return;
            }
            on_tool_message_finished(&ctx, result).await;
        });
        true
    } else {
        false
    }
}
