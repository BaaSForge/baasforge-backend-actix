//! # Tool module standard (one tool per file)
//!
//! Every tool module must provide:
//!
//! - **`TOOL_NAME: &'static str`** — Unique tool name for the LLM.
//! - **`MESSAGE_TYPE: ChatMessageType`** — Message type for queue/DB messages produced by this tool.
//! - **`spec() -> ToolSpec`** — Name, description, and JSON schema for parameters (for LLM declarations).
//! - **`handler(...)`** — Async fn that processes a consumed queue message (same signature for all tools).
//! - **`payload_from_args(args: Value) -> Result<ChatMessagePayload, BoxError>`** — Builds queue payload from LLM args (used by [`tool_context_for_chat_session`]).
//! - **`gemini_declaration() -> FunctionDeclaration`** — Gemini declaration with parameter schema (used by [`to_gemini_declarations`]).
//!
//! To add a new tool: add `mod my_tool;`, add `my_tool::spec()` to [`all_tool_specs`],
//! add `(my_tool::MESSAGE_TYPE, my_tool::handler)` to [`tool_message_handlers`],
//! add `(my_tool::TOOL_NAME, my_tool::MESSAGE_TYPE, my_tool::payload_from_args)` to [`tool_queue_registry`],
//! and add `my_tool::gemini_declaration` to [`gemini_declaration_fns`].

mod load_latest_production_release_units;
mod load_latest_releases;

/// Re-exported for use in [`crate::models::chat_messages::ChatMessagePayload`].
pub use load_latest_production_release_units::ToolFetchRelevantUnitsPayload;
pub use load_latest_releases::ToolFetchLatestReleasesPayload;

use crate::database::{block_on_db_tokio, DbPool};
use crate::libs::ids::new_uuid;
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageStatus, ChatMessageType, ChatSessionRoleType,
};
use crate::routes::chat::message_queue::{self as chat_message_queue, ChatSessionQueueMessage};
use crate::routes::ws::events::{ChatSessionMessageStatusChangePayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::schema::chat_messages;
use crate::services::message_queue::SharedRabbitPublisher;
use diesel::prelude::*;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Callback type for queue-based tools: each is invoked with raw args and returns the new message id.
pub type OnToolCall =
    Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Uuid, BoxError>> + Send>> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// Context for tool execution (callbacks keyed by TOOL_NAME). Set once per request via [`with_tool_context`].
#[derive(Clone, Default)]
pub struct ToolContext {
    /// Map from tool name (e.g. each module’s `TOOL_NAME`) to callback that publishes to the queue.
    pub callbacks: HashMap<String, OnToolCall>,
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

/// Function type each queue-based tool implements: args from LLM → payload for the queue message.
type PayloadFromArgsFn = fn(Value) -> Result<ChatMessagePayload, BoxError>;

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
    ]
}

/// Builds a [`ToolContext`] with one callback per tool in [`tool_queue_registry`]. Each callback
/// converts args to a queue payload and publishes. Use at the entry point (e.g. processor) before
/// calling [`with_tool_context`]. `invoking_message_id` is the AI message that requested the tool
/// calls; tool messages will reference it so the parent can be set to Completed/Failed when all are done.
pub fn tool_context_for_chat_session(
    publisher: Arc<SharedRabbitPublisher>,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
    invoking_message_id: Uuid,
) -> ToolContext {
    let mut callbacks = HashMap::new();
    for (name, message_type, payload_from_args) in tool_queue_registry() {
        let publisher = publisher.clone();
        let message_type = message_type.clone();
        let payload_from_args = *payload_from_args;
        let invoking_message_id = invoking_message_id;
        callbacks.insert(
            (*name).to_string(),
            Arc::new(move |args: Value| {
                let publisher = publisher.clone();
                let user_id = user_id;
                let project_id = project_id;
                let session_id = session_id;
                let message_type = message_type.clone();
                let payload_from_args = payload_from_args;
                let invoking_message_id = invoking_message_id;
                Box::pin(async move {
                    let response_message_id = new_uuid();
                    let payload = payload_from_args(args).map_err(|e| -> BoxError { e })?;
                    let message = ChatSessionQueueMessage {
                        user_id: Some(user_id),
                        role: ChatSessionRoleType::Ai,
                        type_: message_type,
                        payload: Some(payload),
                        response_message_id,
                        retry_num: 0,
                        invoking_message_id: Some(invoking_message_id),
                    };
                    chat_message_queue::publish_with_publisher(
                        &publisher,
                        project_id,
                        session_id,
                        &message,
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

pub fn all_tool_specs() -> Vec<ToolSpec> {
    vec![
        load_latest_production_release_units::spec(),
        load_latest_releases::spec(),
    ]
}

/// Single entry point for the LLM layer: runs queue-based tools via the callback registered under `name`.
/// All tools publish to RabbitMQ; worker creates message and spawns handler.
pub async fn execute_tool_async(name: &str, args: Value) -> Result<String, BoxError> {
    let cb = TOOL_CONTEXT
        .try_with(|ctx| ctx.callbacks.get(name).cloned())
        .map_err(|_| "tools: TOOL_CONTEXT not set (call with_tool_context first)".to_string())?
        .ok_or_else(|| format!("Unknown tool or callback not registered: {}", name))?;
    cb(args).await?;
    Ok("Request queued.".to_string())
}

pub fn to_openai_functions() -> Vec<openai::chat::ChatCompletionFunctionDefinition> {
    all_tool_specs()
        .into_iter()
        .map(|t| openai::chat::ChatCompletionFunctionDefinition {
            name: t.name.to_string(),
            description: Some(t.description.to_string()),
            parameters: Some(t.parameters),
        })
        .collect()
}

/// List of tool functions that produce a Gemini declaration (with parameter schema). Add new tools here when registering.
fn gemini_declaration_fns() -> &'static [fn() -> gemini_rust::tools::model::FunctionDeclaration] {
    &[
        load_latest_production_release_units::gemini_declaration,
        load_latest_releases::gemini_declaration,
    ]
}

/// Gemini declarations with parameter schemas so the model receives required arguments (e.g. `query`).
pub fn to_gemini_declarations() -> Vec<gemini_rust::tools::model::FunctionDeclaration> {
    gemini_declaration_fns().iter().map(|f| f()).collect()
}

// ----- Tool message handlers (runner → worker) -----

type RedisConn = Arc<Mutex<redis::aio::MultiplexedConnection>>;

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
    /// Tool message payload from the queue (same as stored on the message). Handlers can use
    /// tool-call args without querying the DB.
    pub payload: Option<Value>,
    /// When set, this tool message was invoked by the AI message with this id; when all such tool messages are done, that message's status is updated to Completed or Failed.
    pub invoking_message_id: Option<Uuid>,
}

/// Type of a handler that processes a tool message (e.g. after a queue message is consumed).
pub type ToolMessageHandler =
    Arc<dyn Fn(ToolMessageContext) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Wraps an async handler that takes payload into a [`ToolMessageHandler`].
fn wrap_handler_with_payload<F, Fut>(handler: F) -> ToolMessageHandler
where
    F: Fn(DbPool, RedisConn, Uuid, Uuid, Uuid, Uuid, Option<Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
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
        ))
    })
}

/// List of (MESSAGE_TYPE, handler) for all queue-based tools. Add new tools here.
fn tool_message_handlers() -> &'static [(ChatMessageType, ToolMessageHandler)] {
    static HANDLERS: OnceLock<Vec<(ChatMessageType, ToolMessageHandler)>> = OnceLock::new();
    HANDLERS.get_or_init(|| {
        vec![
            (
                load_latest_production_release_units::MESSAGE_TYPE,
                wrap_handler_with_payload(load_latest_production_release_units::handler),
            ),
            (
                load_latest_releases::MESSAGE_TYPE,
                wrap_handler_with_payload(load_latest_releases::handler),
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

/// After a tool message completes (success or failure), check whether the invoking AI message
/// has all its tool messages done; if so, set the AI message status to Completed or Failed.
pub async fn maybe_complete_invoking_message(
    pool: &DbPool,
    redis_conn: &RedisConn,
    invoking_message_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
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
    let topic = format!("chat_session:{}:{}:{}", user_id, project_id, session_id);
    let event = WsServerEvent::ChatSessionMessageStatusChange(
        ChatSessionMessageStatusChangePayload {
            message_id: invoking_message_id,
            status: new_status,
        },
    );
    let mut guard = redis_conn.lock().await;
    if let Err(e) = publish_event_to_channel(&mut *guard, &topic, &event).await {
        tracing::warn!(
            "maybe_complete_invoking_message: failed to publish status change: {}",
            e
        );
    }
}

/// Spawns the appropriate tool handler for the given message type. Does nothing if the type
/// has no registered handler. Call this from the runner instead of matching on each tool type.
/// `payload` should be the tool message payload from the queue (e.g. `ChatMessagePayload` as JSON).
pub fn spawn_tool_message_handler(
    type_: ChatMessageType,
    ctx: ToolMessageContext,
) -> bool {
    if let Some(handler) = tool_message_handler(type_) {
        tokio::spawn(async move {
            handler(ctx.clone()).await;
            if let Some(inv_id) = ctx.invoking_message_id {
                maybe_complete_invoking_message(
                    &ctx.pool,
                    &ctx.redis_conn,
                    inv_id,
                    ctx.user_id,
                    ctx.project_id,
                    ctx.session_id,
                )
                .await;
            }
        });
        true
    } else {
        false
    }
}
