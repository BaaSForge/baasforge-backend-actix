use crate::database::DbPool;
use crate::models::projects::ProjectUnitResponse;
use crate::services::units::api::metadata::TypeScriptJsonSchemaPair;
use crate::services::units::workflow::metadata::WorkflowUnitMetadata;

#[cfg(feature = "unit_technical_reviewer")]
pub async fn build_workflow_type_meta(
    pool: &DbPool,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    code: &str,
) -> WorkflowUnitMetadata {
    use crate::database::block_on_db_tokio;
    use crate::services::chat_worker::db::load_effective_units_for_chat_session;

    let units = block_on_db_tokio(pool, move |conn| {
        load_effective_units_for_chat_session(conn, project_id, session_id)
    })
    .await
    .unwrap_or_default();

    extract_workflow_type_metadata(code, &units)
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub async fn build_workflow_type_meta(
    _pool: &DbPool,
    _project_id: uuid::Uuid,
    _session_id: uuid::Uuid,
    _code: &str,
) -> WorkflowUnitMetadata {
    WorkflowUnitMetadata::default()
}

#[cfg(feature = "unit_technical_reviewer")]
pub fn extract_workflow_type_metadata(
    code: &str,
    units: &[ProjectUnitResponse],
) -> WorkflowUnitMetadata {
    use crate::services::units::type_extractor::{
        build_units_vfs, extract_single_type, get_exported_type_names_from_source,
    };
    use std::collections::HashMap;

    let vfs = build_units_vfs(units);
    let pair = |ts: Option<String>| TypeScriptJsonSchemaPair {
        typescript: ts,
        json_schema: Some(String::new()),
    };

    let all_names = get_exported_type_names_from_source(code);

    let mut handler_inputs: HashMap<String, TypeScriptJsonSchemaPair> = HashMap::new();
    let mut handler_outputs: HashMap<String, TypeScriptJsonSchemaPair> = HashMap::new();

    for name in &all_names {
        if name == "Input" || name == "Output" {
            continue;
        }
        if let Some(handler) = name.strip_suffix("Input") {
            if !handler.is_empty() {
                handler_inputs.insert(
                    handler.to_string(),
                    pair(extract_single_type(name, code, &vfs)),
                );
            }
        } else if let Some(handler) = name.strip_suffix("Output") {
            if !handler.is_empty() {
                handler_outputs.insert(
                    handler.to_string(),
                    pair(extract_single_type(name, code, &vfs)),
                );
            }
        }
    }

    let mut meta = WorkflowUnitMetadata {
        input: pair(extract_single_type("Input", code, &vfs)),
        output: pair(extract_single_type("Output", code, &vfs)),
        handler_inputs,
        handler_outputs,
    };
    enrich_metadata_json_schema(&mut meta);
    meta
}

#[cfg(feature = "unit_technical_reviewer")]
fn enrich_metadata_json_schema(meta: &mut WorkflowUnitMetadata) {
    use crate::services::units::typescript_json_schema::enrich_type_script_json_schema_pair;

    enrich_type_script_json_schema_pair(&mut meta.input, None);
    enrich_type_script_json_schema_pair(&mut meta.output, None);
    for p in meta.handler_inputs.values_mut() {
        enrich_type_script_json_schema_pair(p, None);
    }
    for p in meta.handler_outputs.values_mut() {
        enrich_type_script_json_schema_pair(p, None);
    }
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn extract_workflow_type_metadata(
    _code: &str,
    _units: &[ProjectUnitResponse],
) -> WorkflowUnitMetadata {
    WorkflowUnitMetadata::default()
}
