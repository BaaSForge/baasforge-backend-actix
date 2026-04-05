use baal_backend_actix::services::unit_technical_reviewer::runner;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    runner::run().await
}
