// Resolves the three exported contract types (`RequestBody`, `QueryParams`,
// `ResponseBody`) from an API unit's source into deeply-resolved, separated
// TypeScript declarations — using the generic machinery in
// `crate::services::units::type_extractor`.
//
// `build_api_type_meta` is the async entry point: it loads the session's
// effective units from the database and then runs the synchronous extraction.

use crate::database::DbPool;
use crate::models::projects::ProjectUnitResponse;
use crate::services::units::api::metadata::{ApiUnitMetadata, TypeScriptJsonSchemaPair};

// ── Async orchestration (loads units from DB) ─────────────────────────────────

/// Load effective units for the session and extract API type metadata.
///
/// Full implementation compiled only with the `unit_technical_reviewer` feature.
/// Returns `ApiUnitMetadata::default()` otherwise (no DB call, no OXC usage).
#[cfg(feature = "unit_technical_reviewer")]
pub async fn build_api_type_meta(
    pool: &DbPool,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    code: &str,
) -> ApiUnitMetadata {
    use crate::database::block_on_db_tokio;
    use crate::services::chat_worker::db::load_effective_units_for_chat_session;

    let units = block_on_db_tokio(pool, move |conn| {
        load_effective_units_for_chat_session(conn, project_id, session_id)
    })
    .await
    .unwrap_or_default();

    extract_api_type_metadata(code, &units)
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub async fn build_api_type_meta(
    _pool: &DbPool,
    _project_id: uuid::Uuid,
    _session_id: uuid::Uuid,
    _code: &str,
) -> ApiUnitMetadata {
    ApiUnitMetadata::default()
}

// ── Synchronous extraction (pure computation given pre-loaded units) ───────────

/// Extract the three API contract types from `code`, resolving all transitive
/// dependencies from `units`.
///
/// Full implementation compiled only with the `unit_technical_reviewer` feature.
/// Returns `ApiUnitMetadata::default()` otherwise.
#[cfg(feature = "unit_technical_reviewer")]
pub fn extract_api_type_metadata(code: &str, units: &[ProjectUnitResponse]) -> ApiUnitMetadata {
    use crate::services::units::type_extractor::{build_units_vfs, extract_single_type};

    let vfs = build_units_vfs(units);
    let pair = |ts: Option<String>| TypeScriptJsonSchemaPair {
        typescript: ts,
        json_schema: Some(String::new()),
    };

    let mut meta = ApiUnitMetadata {
        method: None,
        timeout_seconds: None,
        request_body: pair(extract_single_type("RequestBody", code, &vfs)),
        request_params: pair(extract_single_type("QueryParams", code, &vfs)),
        response_body: pair(extract_single_type("ResponseBody", code, &vfs)),
    };
    enrich_metadata_json_schema(&mut meta);
    meta
}

#[cfg(feature = "unit_technical_reviewer")]
fn enrich_metadata_json_schema(meta: &mut ApiUnitMetadata) {
    use crate::services::units::typescript_json_schema::enrich_type_script_json_schema_pair;

    enrich_type_script_json_schema_pair(&mut meta.request_body, None);
    enrich_type_script_json_schema_pair(&mut meta.request_params, None);
    enrich_type_script_json_schema_pair(&mut meta.response_body, None);
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn extract_api_type_metadata(_code: &str, _units: &[ProjectUnitResponse]) -> ApiUnitMetadata {
    ApiUnitMetadata::default()
}
