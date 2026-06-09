use crate::database::{BlockingDbError, DbPool, block_on_db_tokio};
use crate::models::chat_messages::{
    ChatMessagePayload, ChatMessageResponse, ChatMessageRow, ChatMessageStatus, ChatMessageType,
};
use crate::routes::ws::events::{ChatSessionMessageUpdatedPayload, WsServerEvent};
use crate::routes::ws::redis::publish_event_to_channel;
use crate::routes::ws::user_channel;
use crate::schema::chat_messages;
use crate::services::message_queue::SharedRabbitPublisher;
use crate::tsgo::package_type_definition_files::{
    PackageTypeDefinitionFiles, ResolvePackageTypeDefinitionFilesResult,
    resolve_package_type_definition_files,
};
use diesel::prelude::*;
use gemini_rust::tools::model::FunctionDeclaration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{BoxError, ToolSpec};

pub const ARG_PACKAGES_DESCRIPTION: &str =
    "Exact npm package names from the runtime type-definition files.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolGetPackagesTypeDefinitionFilesArgs {
    #[schemars(description = ARG_PACKAGES_DESCRIPTION)]
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolGetPackagesTypeDefinitionFilesPayload {
    #[serde(default)]
    pub args: ToolGetPackagesTypeDefinitionFilesArgs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_type_definition_files: Option<Vec<PackageTypeDefinitionFiles>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_packages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolGetPackagesTypeDefinitionFilesArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolGetPackagesTypeDefinitionFiles;

pub const TOOL_NAME: &str = "get_packages_type_definition_files";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Load TypeScript definition file paths (.d.ts, .d.cts, and package.json) for npm packages. Pass exact package names. Use before importing unfamiliar APIs so you match real exports and types. Returns paths only — call `get_package_type_definition_file_contents` to load each file's content and inspect the types inside.",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "packages": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "description": ARG_PACKAGES_DESCRIPTION
            }
        },
        "required": ["packages"],
        "additionalProperties": false
    })
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolGetPackagesTypeDefinitionFilesArgs>(args)?;
    Ok(ChatMessagePayload::ToolGetPackagesTypeDefinitionFiles(
        ToolGetPackagesTypeDefinitionFilesPayload {
            args,
            package_type_definition_files: None,
            unknown_packages: vec![],
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

/// Hydrates file paths from the embedded runtime manifest (no DB). Used when sending tool
/// results to the LLM and by the platform fill API — not when persisting tool rows.
pub fn fill_package_type_definition_files_payload(
    payload: &ToolGetPackagesTypeDefinitionFilesPayload,
) -> ToolGetPackagesTypeDefinitionFilesPayload {
    let resolved = resolve_package_type_definition_files(&payload.args.packages);
    build_filled_payload(payload, resolved)
}

/// JSON tool-result body for upstream LLM history (static manifest lookup, not DB).
pub fn upstream_tool_result_content(payload: &ToolGetPackagesTypeDefinitionFilesPayload) -> String {
    serde_json::to_string(&fill_package_type_definition_files_payload(payload)).unwrap_or_default()
}

pub fn fill_package_type_definition_files_from_packages(
    packages: &[String],
) -> ToolGetPackagesTypeDefinitionFilesPayload {
    let resolved = resolve_package_type_definition_files(packages);
    build_filled_payload(
        &ToolGetPackagesTypeDefinitionFilesPayload {
            args: ToolGetPackagesTypeDefinitionFilesArgs {
                packages: packages.to_vec(),
            },
            package_type_definition_files: None,
            unknown_packages: vec![],
            tool_call_id: None,
            tool_name: Some(TOOL_NAME.to_string()),
            error: None,
        },
        resolved,
    )
}

fn build_filled_payload(
    base: &ToolGetPackagesTypeDefinitionFilesPayload,
    resolved: ResolvePackageTypeDefinitionFilesResult,
) -> ToolGetPackagesTypeDefinitionFilesPayload {
    let unknown_error = if resolved.unknown_packages.is_empty() {
        None
    } else {
        Some(format!(
            "Unknown package name(s): {}.",
            resolved.unknown_packages.join(", ")
        ))
    };
    ToolGetPackagesTypeDefinitionFilesPayload {
        args: base.args.clone(),
        package_type_definition_files: Some(resolved.package_type_definition_files),
        unknown_packages: resolved.unknown_packages,
        tool_call_id: base.tool_call_id.clone(),
        tool_name: base.tool_name.clone(),
        error: base.error.clone().or(unknown_error),
    }
}

fn validate_packages(packages: &[String]) -> Result<(), String> {
    let non_empty: Vec<&str> = packages
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if non_empty.is_empty() {
        return Err("`packages` must contain at least one non-empty package name.".to_string());
    }
    Ok(())
}

pub async fn handler(
    pool: DbPool,
    redis_conn: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    _project_id: Uuid,
    _session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    payload: Option<ChatMessagePayload>,
    _rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let existing = match payload {
        Some(ChatMessagePayload::ToolGetPackagesTypeDefinitionFiles(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .unwrap_or_default();
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());

    let validation_error = validate_packages(&args.packages).err().or_else(|| {
        let unknown = resolve_package_type_definition_files(&args.packages).unknown_packages;
        if unknown.is_empty() {
            None
        } else {
            Some(format!(
                "Unknown package name(s): {}.",
                unknown.join(", ")
            ))
        }
    });

    let args_only_payload = ToolGetPackagesTypeDefinitionFilesPayload {
        args,
        package_type_definition_files: None,
        unknown_packages: vec![],
        tool_call_id,
        tool_name,
        error: validation_error,
    };
    let payload_value = serde_json::to_value(&args_only_payload).map_err(|e| -> BoxError {
        format!("serialize get_packages_type_definition_files: {}", e).into()
    })?;

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
    .map_err(|e: BlockingDbError| -> BoxError { Box::new(e) })?;

    let channel = user_channel(user_id);
    let message: ChatMessageResponse = updated_row.into();
    let event = WsServerEvent::ChatSessionMessageUpdated(ChatSessionMessageUpdatedPayload {
        project_id: _project_id,
        session_id: _session_id,
        message,
    });
    let mut guard = redis_conn.lock().await;
    publish_event_to_channel(&mut *guard, &channel, &event)
        .await
        .map_err(|e| -> BoxError { e.to_string().into() })?;
    Ok(())
}
