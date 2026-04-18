pub async fn build_queue_worker_type_meta(
    _pool: &crate::database::DbPool,
    _project_id: uuid::Uuid,
    _session_id: uuid::Uuid,
    _code: &str,
) -> super::metadata::QueueWorkerUnitMetadata {
    super::metadata::QueueWorkerUnitMetadata {}
}
