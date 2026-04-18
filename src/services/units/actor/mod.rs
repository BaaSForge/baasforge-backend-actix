pub mod definitions;
pub mod metadata;
pub mod rules;
pub mod type_extractor;

pub use definitions::{BRIEF, COMMUNICATION, DETAIL, METADATA, TITLE};

pub async fn build_unit_metadata(
    pool: &crate::database::DbPool,
    project_id: uuid::Uuid,
    session_id: uuid::Uuid,
    code: &str,
) -> Option<super::UnitMetadata> {
    let meta = type_extractor::build_actor_type_meta(pool, project_id, session_id, code).await;
    Some(super::UnitMetadata::Actor(meta))
}
