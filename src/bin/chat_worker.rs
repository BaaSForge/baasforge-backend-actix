use baal_backend_actix::services::chat_worker::runner;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    runner::run().await
}
