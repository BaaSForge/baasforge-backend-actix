use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use tracing::{info, warn};

use crate::configs::Settings;
use crate::database::{block_on_db_tokio, establish_pool};
use crate::services::message_queue::{wait_for_shared_publisher, PublishError};
use crate::services::platform_cost_calculate::db::{self, DueProjectRow};
use crate::services::platform_cost_calculate::queue::{
    self, PlatformCostCalculateMessage, PLATFORM_COST_CALCULATE_EXCHANGE,
};

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    let settings = Settings::new()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })?;

    let pool = establish_pool(
        settings.pg_database_url.clone(),
        settings.database_pool_size,
        settings.database_connection_timeout_secs,
        settings.database_pool_idle_timeout_secs,
    );

    let rabbit_recovery_wait =
        Duration::from_secs(settings.rabbit_worker_recovery_wait_secs);
    let rabbit_connect_retry =
        Duration::from_secs(settings.rabbit_connect_retry_interval_secs);

    let publisher = wait_for_shared_publisher(
        &settings.rabbit_url,
        PLATFORM_COST_CALCULATE_EXCHANGE,
        rabbit_recovery_wait,
        rabbit_connect_retry,
    )
    .await;

    let now = Utc::now();
    let window_minutes = settings.platform_cost_calculate_window_minutes;
    let next_at = now + ChronoDuration::minutes(window_minutes);
    let batch_size = settings.platform_cost_calculate_scheduler_batch_size as i64;

    let due = block_on_db_tokio(&pool, move |conn| db::list_due_projects(conn, now, batch_size))
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })?;

    info!(
        due_count = due.len(),
        batch_size,
        "platform cost calculate scheduler starting"
    );

    let mut published = 0_u32;
    let mut advanced = 0_u32;

    for row in due {
        match schedule_project(&pool, &publisher, &row, now, next_at).await {
            Ok(true) => {
                published += 1;
                advanced += 1;
            }
            Ok(false) => {}
            Err(e) => {
                warn!(error = %e, project_id = %row.id, "failed to schedule platform cost calculate");
            }
        }
    }

    info!(
        published,
        advanced,
        "platform cost calculate scheduler finished"
    );
    Ok(())
}

async fn schedule_project(
    pool: &crate::database::DbPool,
    publisher: &Arc<crate::services::message_queue::SharedRabbitPublisher>,
    row: &DueProjectRow,
    now: chrono::DateTime<Utc>,
    next_at: chrono::DateTime<Utc>,
) -> Result<bool, String> {
    let period_start = row.last_cost_calculated_at.unwrap_or(now);
    let duration_seconds = (next_at - period_start).num_seconds().max(0);
    if duration_seconds == 0 {
        return Ok(false);
    }

    let message = PlatformCostCalculateMessage {
        project_id: row.id,
        owner_user_id: row.owner_user_id,
        period_start,
        period_end: next_at,
        duration_seconds,
        retry_num: 0,
    };

    queue::publish_cost_calculate_message(publisher, &message)
        .await
        .map_err(|e: PublishError| e.to_string())?;

    let project_id = row.id;
    let expected_previous = row.last_cost_calculated_at;
    let advanced = block_on_db_tokio(pool, move |conn| {
        db::advance_platform_cost_window(conn, project_id, expected_previous, next_at)
    })
    .await
    .map_err(|e| e.to_string())?;

    if !advanced {
        warn!(
            project_id = %row.id,
            "published cost calculate message but project cursor was not advanced"
        );
    }

    Ok(advanced)
}
