use crate::database::DbPool;
use crate::models::projects::ProjectUnitResponse;
use crate::services::units::api::metadata::TypeScriptJsonSchemaPair;
use crate::services::units::actor::metadata::ActorUnitMetadata;

#[cfg(feature = "unit_technical_reviewer")]
pub async fn build_actor_type_meta(
    pool: &DbPool,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    code: &str,
) -> ActorUnitMetadata {
    use crate::database::block_on_db_tokio;
    use crate::services::chat_worker::db::load_effective_units_for_chat_session;

    let units = block_on_db_tokio(pool, move |conn| {
        load_effective_units_for_chat_session(conn, project_id, session_id)
    })
    .await
    .unwrap_or_default();

    extract_actor_type_metadata(code, &units)
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub async fn build_actor_type_meta(
    _pool: &DbPool,
    _project_id: uuid::Uuid,
    _session_id: uuid::Uuid,
    _code: &str,
) -> ActorUnitMetadata {
    ActorUnitMetadata::default()
}

const INTERNAL_HANDLER_PREFIXES: &[&str] = &["OnAlarm", "OnDeadLetter"];

#[cfg(feature = "unit_technical_reviewer")]
pub fn extract_actor_type_metadata(
    code: &str,
    units: &[ProjectUnitResponse],
) -> ActorUnitMetadata {
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
        if name == "State" {
            continue;
        }
        let is_internal = INTERNAL_HANDLER_PREFIXES
            .iter()
            .any(|p| name.starts_with(p));
        if is_internal {
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

    ActorUnitMetadata {
        state: pair(extract_single_type("State", code, &vfs)),
        handler_inputs,
        handler_outputs,
    }
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn extract_actor_type_metadata(
    _code: &str,
    _units: &[ProjectUnitResponse],
) -> ActorUnitMetadata {
    ActorUnitMetadata::default()
}
