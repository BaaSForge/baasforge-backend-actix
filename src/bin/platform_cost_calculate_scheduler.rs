use baal_backend_actix::services::platform_cost_calculate::scheduler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    scheduler::run().await
}
