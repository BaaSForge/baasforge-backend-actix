pub mod definitions;
pub mod metadata;
pub mod rules;
pub mod type_extractor;

pub use definitions::{BRIEF, CODE_EXAMPLES, COMMUNICATION, DETAIL, METADATA, RUNTIME, TITLE};

/// Build the persisted `UnitMetadata` for an API unit after all checks pass.
///
/// Extracts `RequestBody`, `QueryParams`, and `ResponseBody` TypeScript declarations
/// and stores them alongside the HTTP method and optional timeout.
pub async fn build_unit_metadata(
    pool: &crate::database::DbPool,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    code: &str,
    api_method: Option<metadata::HttpMethod>,
    api_timeout_seconds: Option<u32>,
) -> Option<super::UnitMetadata> {
    let mut meta = type_extractor::build_api_type_meta(pool, project_id, session_id, code).await;
    meta.method = api_method;
    meta.timeout_seconds = api_timeout_seconds;
    Some(super::UnitMetadata::Api(meta))
}
