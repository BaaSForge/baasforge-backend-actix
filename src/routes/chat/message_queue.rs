use lapin::Consumer;
use lapin::Connection;

#[allow(dead_code)]
pub async fn publish(
    rabbit_url: &str,
    project_id: i64,
    session_id: i64,
    payload: &[u8],
) -> Result<(), lapin::Error> {
    crate::services::rabbit::publish_chat_session_message(
        rabbit_url,
        project_id,
        session_id,
        payload,
    )
    .await
}

pub async fn create_consumer(
    rabbit_url: &str,
    project_id: i64,
    session_id: i64,
) -> Result<(Connection, Consumer), lapin::Error> {
    crate::services::rabbit::create_chat_session_consumer(rabbit_url, project_id, session_id).await
}
