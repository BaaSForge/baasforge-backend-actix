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
    PackageTypeDefinitionFileContent, ResolvePackageTypeDefinitionFileContentsResult,
    resolve_package_type_definition_file_contents, validate_node_modules_file_paths,
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

pub const ARG_FILES_DESCRIPTION: &str =
    "Type-definition file paths from `get_packages_type_definition_files`. Each path must start with `node_modules/` (e.g. `node_modules/zod/index.d.ts`). Copy paths verbatim.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
pub struct ToolGetPackageTypeDefinitionFileContentsArgs {
    #[schemars(description = ARG_FILES_DESCRIPTION)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolGetPackageTypeDefinitionFileContentsPayload {
    #[serde(default)]
    pub args: ToolGetPackageTypeDefinitionFileContentsArgs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_type_definition_file_contents: Option<Vec<PackageTypeDefinitionFileContent>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn gemini_declaration() -> FunctionDeclaration {
    FunctionDeclaration::new(TOOL_NAME, spec().description, None)
        .with_parameters::<ToolGetPackageTypeDefinitionFileContentsArgs>()
}

pub const MESSAGE_TYPE: ChatMessageType = ChatMessageType::ToolGetPackageTypeDefinitionFileContents;

pub const TOOL_NAME: &str = "get_package_type_definition_file_contents";

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME,
        description: "Load TypeScript definition file contents for paths returned by `get_packages_type_definition_files`. Each path must start with `node_modules/`. Use to inspect exports and types inside each `.d.ts`, `.d.cts`, or `package.json` before writing imports.",
        parameters: params_schema(),
    }
}

fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "files": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "description": ARG_FILES_DESCRIPTION
            }
        },
        "required": ["files"],
        "additionalProperties": false
    })
}

pub fn payload_from_args(
    args: Value,
    tool_call_id: Option<String>,
    tool_name: &str,
) -> Result<ChatMessagePayload, BoxError> {
    let args = serde_json::from_value::<ToolGetPackageTypeDefinitionFileContentsArgs>(args)?;
    Ok(ChatMessagePayload::ToolGetPackageTypeDefinitionFileContents(
        ToolGetPackageTypeDefinitionFileContentsPayload {
            args,
            package_type_definition_file_contents: None,
            unknown_files: vec![],
            tool_call_id,
            tool_name: Some(tool_name.to_string()),
            error: None,
        },
    ))
}

/// Hydrates file contents from the embedded runtime manifest (no DB). Used when sending tool
/// results to the LLM and by the platform fill API — not when persisting tool rows.
pub fn fill_package_type_definition_file_contents_payload(
    payload: &ToolGetPackageTypeDefinitionFileContentsPayload,
) -> ToolGetPackageTypeDefinitionFileContentsPayload {
    let resolved = resolve_package_type_definition_file_contents(&payload.args.files);
    build_filled_payload(payload, resolved)
}

/// JSON tool-result body for upstream LLM history (static manifest lookup, not DB).
pub fn upstream_tool_result_content(
    payload: &ToolGetPackageTypeDefinitionFileContentsPayload,
) -> String {
    serde_json::to_string(&fill_package_type_definition_file_contents_payload(payload))
        .unwrap_or_default()
}

pub fn fill_package_type_definition_file_contents_from_files(
    files: &[String],
) -> ToolGetPackageTypeDefinitionFileContentsPayload {
    let resolved = resolve_package_type_definition_file_contents(files);
    build_filled_payload(
        &ToolGetPackageTypeDefinitionFileContentsPayload {
            args: ToolGetPackageTypeDefinitionFileContentsArgs {
                files: files.to_vec(),
            },
            package_type_definition_file_contents: None,
            unknown_files: vec![],
            tool_call_id: None,
            tool_name: Some(TOOL_NAME.to_string()),
            error: None,
        },
        resolved,
    )
}

fn build_filled_payload(
    base: &ToolGetPackageTypeDefinitionFileContentsPayload,
    resolved: ResolvePackageTypeDefinitionFileContentsResult,
) -> ToolGetPackageTypeDefinitionFileContentsPayload {
    let unknown_error = if resolved.unknown_files.is_empty() {
        None
    } else {
        Some(format!(
            "Unknown file path(s): {}.",
            resolved.unknown_files.join(", ")
        ))
    };
    ToolGetPackageTypeDefinitionFileContentsPayload {
        args: base.args.clone(),
        package_type_definition_file_contents: Some(resolved.package_type_definition_file_contents),
        unknown_files: resolved.unknown_files,
        tool_call_id: base.tool_call_id.clone(),
        tool_name: base.tool_name.clone(),
        error: base.error.clone().or(unknown_error),
    }
}

fn validate_files(files: &[String]) -> Result<(), String> {
    validate_node_modules_file_paths(files)
}

pub async fn handler(
    pool: DbPool,
    redis_conn: crate::services::redis_conn::RedisConn,
    _project_id: Uuid,
    _session_id: Uuid,
    message_id: Uuid,
    user_id: Uuid,
    payload: Option<ChatMessagePayload>,
    _rabbit_publisher: Option<Arc<SharedRabbitPublisher>>,
) -> Result<(), BoxError> {
    let existing = match payload {
        Some(ChatMessagePayload::ToolGetPackageTypeDefinitionFileContents(p)) => Some(p),
        _ => None,
    };
    let args = existing
        .as_ref()
        .map(|p| p.args.clone())
        .unwrap_or_default();
    let tool_call_id = existing.as_ref().and_then(|p| p.tool_call_id.clone());
    let tool_name = Some(TOOL_NAME.to_string());

    let validation_error = validate_files(&args.files).err().or_else(|| {
        let unknown = resolve_package_type_definition_file_contents(&args.files).unknown_files;
        if unknown.is_empty() {
            None
        } else {
            Some(format!("Unknown file path(s): {}.", unknown.join(", ")))
        }
    });

    let args_only_payload = ToolGetPackageTypeDefinitionFileContentsPayload {
        args,
        package_type_definition_file_contents: None,
        unknown_files: vec![],
        tool_call_id,
        tool_name,
        error: validation_error,
    };
    let payload_value = serde_json::to_value(&args_only_payload).map_err(|e| -> BoxError {
        format!("serialize get_package_type_definition_file_contents: {}", e).into()
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
    crate::routes::ws::redis::publish_event_on_conn(redis_conn.clone(), &channel, &event)
        .await
        .map_err(|e| -> BoxError { e.to_string().into() })?;
    Ok(())
}
