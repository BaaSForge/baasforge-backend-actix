use crate::database::DbPool;
use crate::models::projects::ProjectUnitResponse;
#[cfg(feature = "unit_technical_reviewer")]
use crate::services::units::api::metadata::TypeScriptJsonSchemaPair;
use crate::services::units::queue_worker::metadata::QueueWorkerUnitMetadata;

#[cfg(feature = "unit_technical_reviewer")]
pub async fn build_queue_worker_type_meta(
    pool: &DbPool,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    code: &str,
) -> QueueWorkerUnitMetadata {
    use crate::database::block_on_db_tokio;
    use crate::services::chat_worker::db::load_effective_units_for_chat_session;

    let units = block_on_db_tokio(pool, move |conn| {
        load_effective_units_for_chat_session(conn, project_id, session_id)
    })
    .await
    .unwrap_or_default();

    extract_queue_worker_type_metadata(code, &units)
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub async fn build_queue_worker_type_meta(
    _pool: &DbPool,
    _project_id: uuid::Uuid,
    _session_id: uuid::Uuid,
    _code: &str,
) -> QueueWorkerUnitMetadata {
    QueueWorkerUnitMetadata::default()
}

#[cfg(feature = "unit_technical_reviewer")]
pub fn extract_queue_worker_type_metadata(
    code: &str,
    units: &[ProjectUnitResponse],
) -> QueueWorkerUnitMetadata {
    use crate::services::units::type_extractor::{build_units_vfs, extract_single_type};

    let vfs = build_units_vfs(units);
    let pair = |ts: Option<String>| TypeScriptJsonSchemaPair {
        typescript: ts,
        json_schema: Some(String::new()),
    };

    let mut meta = QueueWorkerUnitMetadata {
        input: pair(extract_single_type("Input", code, &vfs)),
        output: pair(extract_single_type("Output", code, &vfs)),
        batch_output: pair(extract_single_type("BatchOutput", code, &vfs)),
    };
    enrich_metadata_json_schema(&mut meta);
    meta
}

#[cfg(feature = "unit_technical_reviewer")]
fn enrich_metadata_json_schema(meta: &mut QueueWorkerUnitMetadata) {
    use crate::services::units::typescript_json_schema::enrich_type_script_json_schema_pair;

    enrich_type_script_json_schema_pair(&mut meta.input, None);
    enrich_type_script_json_schema_pair(&mut meta.output, None);
    enrich_type_script_json_schema_pair(&mut meta.batch_output, None);
}

#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn extract_queue_worker_type_metadata(
    _code: &str,
    _units: &[ProjectUnitResponse],
) -> QueueWorkerUnitMetadata {
    QueueWorkerUnitMetadata::default()
}
