pub mod definitions;
pub mod metadata;
pub mod rules;
pub mod type_extractor;

pub use definitions::{BRIEF, CODE_EXAMPLES, COMMUNICATION, DETAIL, METADATA, TITLE};

/// Build the persisted `UnitMetadata` for an API unit after all checks pass.
///
/// Extracts `RequestBody`, `QueryParams`, and `ResponseBody` TypeScript declarations
/// and stores them alongside the HTTP method.
pub async fn build_unit_metadata(
    pool: &crate::database::DbPool,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    code: &str,
    api_method: Option<metadata::HttpMethod>,
) -> Option<super::UnitMetadata> {
    let mut meta = type_extractor::build_api_type_meta(pool, project_id, session_id, code).await;
    meta.method = api_method;
    Some(super::UnitMetadata::Api(meta))
}
