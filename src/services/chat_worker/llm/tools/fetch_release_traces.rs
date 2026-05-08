use crate::configs::Settings;
use crate::database::{DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::message_queue::SharedRabbitPublisher;
use crate::services::observability::{
    JaegerQueryError, JaegerQueryFilters, MAX_LOOKBACK_MINUTES, ObservabilityService,
    query_jaeger_traces,
};
use diesel::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{BoxError, ToolSpec};
use gemini_rust::tools::model::FunctionDeclaration;

const LIMIT_PER_SERVICE: usize = 10;

fn validate_trace_id(trace_id: &str) -> Result<(), String> {
    if !trace_id
        .chars()
        .all(|c| c.is_ascii_hexdigit())
    {
        return Err(
            "fetch_release_traces: trace_id must be hexadecimal only (0-9, a-f). Do not invent, merge, or re-type ids from the brief—copy the exact trace_id from the observability handoff."
                .to_string(),
        );
    }
    if trace_id.len() != 16 && trace_id.len() != 32 {
        return Err(format!(
            "fetch_release_traces: trace_id must be exactly 16 or 32 hex characters (Jaeger / OpenTelemetry); got length {}. A longer or mixed string usually means the id was corrupted—use only the exact trace_id from the handoff.",
            trace_id.len()
        ));
    }
    Ok(())
}

fn validate_release_id(release_id: &str) -> Result<(), String> {
    Uuid::parse_str(release_id).map_err(|_| {
        format!(
            "fetch_release_traces: release_id must be a UUID (tag `baal.release.id` is the project release id). Got {:?}. Do not pass environment names (e.g. production, stage) or invented values—omit release_id when the handoff only pins trace_id, or pass the exact release_id from analyze_observability_tool args.",
            release_id
        )
    })?;
    Ok(())
}

fn validate_tool_trace_and_release(
    trace_trim: Option<&str>,
    release_trim: Option<&str>,
) -> Result<(), String> {
    if let Some(t) = trace_trim {
        validate_trace_id(t)?;
    }
    if let Some(r) = release_trim {
        validate_release_id(r)?;
    }
    Ok(())
}

fn all_observability_services() -> Vec<ObservabilityService> {
    vec![
        ObservabilityService::ApiServer,
        ObservabilityService::ActorWorker,
        ObservabilityService::QueueWorkerWorker,
        ObservabilityService::WorkflowWorker,
    ]
}

pub const ARG_TRACE_ID_DESCRIPTION: &str = "Optional Jaeger trace id. When set alone, loads that full trace (no `baal.release.id` filter). When set with `release_id`, keeps only spans whose `baal.release.id` tag matches. Omit or empty to search recent traces per service (then `release_id` is required).";

pub const ARG_RELEASE_ID_DESCRIPTION: &str = "Filters spans to `baal.release.id` equal to this value. Required when `trace_id` is omitted (per-service search). Optional when `trace_id` is set.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolFetchReleaseTracesArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = ARG_TRACE_ID_DESCRIPTION)]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = ARG_RELEASE_ID_DESCRIPTION)]
    pub release_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolFetchReleaseTracesPayload {
    pub args: ToolFetchReleaseTracesArgs,
    #[serde(default)]
    pub spans: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolFetchReleaseTracesArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolFetchReleaseTraces;

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let mut args = serde_json::from_value::<ToolFetchReleaseTracesArgs>(args).unwrap_or_default();
    args.release_id = args
        .release_id
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    args.trace_id = args
        .trace_id
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(ChatMessagePayload::ToolFetchReleaseTraces(
        ToolFetchReleaseTracesPayload {
            args,
            spans: vec![],
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "trace_id": {
                "type": "string",
                "description": ARG_TRACE_ID_DESCRIPTION
            },
            "release_id": {
                "type": "string",
                "description": ARG_RELEASE_ID_DESCRIPTION
            }
        }
    })
}

pub const TOOL_NAME: &str = "fetch_release_traces";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Read-only: load Jaeger spans from instrumented processes (15-minute lookback and per-service cap when searching without `trace_id`; by `trace_id`, the full trace is requested without a lookback window). Either pass non-empty `release_id` to match tag `baal.release.id` (required if `trace_id` is omitted), or pass `trace_id` alone to load that trace without a release filter, or pass both to load one trace and filter spans by release.",
        parameters: params_schema(),
    }
}

pub async fn handler(
    pool: DbPool,
    redis_conn: std::sync::Arc<Mutex<redis::aio::MultiplexedConnection>>,
    _project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    payload: Option<ChatMessagePayload>,
    _rabbit_publisher: Option<std::sync::Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let existing = match payload {
        Some(ChatMessagePayload::ToolFetchReleaseTraces(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .unwrap_or_default();
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());

    let release_trim = args
        .release_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let trace_trim = args
        .trace_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if release_trim.is_none() && trace_trim.is_none() {
        let full_payload = ToolFetchReleaseTracesPayload {
            args,
            spans: vec![],
            tool_call_id,
            tool_name,
            error: Some(
                "fetch_release_traces: pass a non-empty `release_id`, or pass `trace_id` (trace-only)."
                    .to_string(),
            ),
        };
        return persist_completed(
            pool,
            redis_conn,
            _project_id,
            session_id,
            message_id,
            user_id,
            full_payload,
        )
        .await;
    }

    if let Err(msg) = validate_tool_trace_and_release(trace_trim, release_trim) {
        let full_payload = ToolFetchReleaseTracesPayload {
            args,
            spans: vec![],
            tool_call_id,
            tool_name,
            error: Some(msg),
        };
        return persist_completed(
            pool,
            redis_conn,
            _project_id,
            session_id,
            message_id,
            user_id,
            full_payload,
        )
        .await;
    }

    let services = all_observability_services();

    let base_url = match Settings::new()
        .map(|s| s.jaeger_query_base_url)
        .map_err(|e| e.to_string())
    {
        Ok(url) => url,
        Err(err_msg) => {
            let full_payload = ToolFetchReleaseTracesPayload {
                args,
                spans: vec![],
                tool_call_id,
                tool_name,
                error: Some(format!(
                    "fetch_release_traces: failed to load settings: {}",
                    err_msg
                )),
            };
            return persist_completed(
                pool,
                redis_conn,
                _project_id,
                session_id,
                message_id,
                user_id,
                full_payload,
            )
            .await;
        }
    };

    let filters = JaegerQueryFilters {
        trace_id: args.trace_id.clone(),
        release_id: release_trim.map(|s| s.to_string()),
        services,
        lookback_minutes: MAX_LOOKBACK_MINUTES,
        limit_per_service: LIMIT_PER_SERVICE,
    };

    let spans = match query_jaeger_traces(&base_url, &filters).await {
        Ok(m) => m
            .into_iter()
            .flat_map(|(_, spans)| spans)
            .collect::<Vec<_>>(),
        Err(e) => {
            let msg = jaeger_err_message(&e);
            let full_payload = ToolFetchReleaseTracesPayload {
                args,
                spans: vec![],
                tool_call_id,
                tool_name,
                error: Some(msg),
            };
            return persist_completed(
                pool,
                redis_conn,
                _project_id,
                session_id,
                message_id,
                user_id,
                full_payload,
            )
            .await;
        }
    };

    let full_payload = ToolFetchReleaseTracesPayload {
        args,
        spans,
        tool_call_id,
        tool_name,
        error: None,
    };
    persist_completed(
        pool,
        redis_conn,
        _project_id,
        session_id,
        message_id,
        user_id,
        full_payload,
    )
    .await
}

fn jaeger_err_message(e: &JaegerQueryError) -> String {
    format!("fetch_release_traces: {}", e)
}

async fn persist_completed(
    pool: DbPool,
    redis_conn: std::sync::Arc<Mutex<redis::aio::MultiplexedConnection>>,
    project_id: Uuid,
    session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    full_payload: ToolFetchReleaseTracesPayload,
) -> Result<(), BoxError> {
    let payload_value = serde_json::to_value(&full_payload)
        .map_err(|e| -> BoxError { format!("serialize fetch_release_traces: {}", e).into() })?;
    let updated_row = block_on_db_tokio(&pool, move |conn| {
        diesel::update(chat_messages::table.filter(chat_messages::id.eq(message_id)))
            .set((
                chat_messages::payload.eq(Some(payload_value)),
                chat_messages::status.eq(ChatMessageStatus::Completed),
            ))
            .returning(ChatMessageRow::as_returning())
            .get_result(conn)
    })
    .await
    .map_err(|e: crate::database::BlockingDbError| -> BoxError { Box::new(e) })?;
    let channel = user_channel(user_id);
    let message: ChatMessageResponse = updated_row.into();
    let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
        project_id,
        session_id,
        message,
    });
    let mut guard = redis_conn.lock().await;
    publish_event_to_channel(&mut *guard, &channel, &event)
        .await
        .map_err(|e| -> BoxError { e.to_string().into() })?;
    Ok(())
}
