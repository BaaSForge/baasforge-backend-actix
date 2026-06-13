use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use crate::libs::ids::new_uuid;
use crate::services::pricing::{
    ENTITY_SECOND_PRICE_UNIT, PLATFORM_KIND, PROJECT_METRIC, STAGE_RUNTIME_METRIC,
    SUBSCRIPTION_METRIC, UNIT_METRIC,
};
use crate::services::usage_publisher::{self, UsageEntry};

pub fn enqueue_platform_usage(
    project_id: Uuid,
    metric: &str,
    entity_seconds: i64,
    recorded_at: DateTime<Utc>,
    details: serde_json::Value,
) {
    if entity_seconds <= 0 {
        return;
    }
    usage_publisher::enqueue(UsageEntry {
        id: new_uuid(),
        project_id,
        recorded_at,
        kind: PLATFORM_KIND.to_string(),
        metric: metric.to_string(),
        quantity: entity_seconds,
        price_unit: ENTITY_SECOND_PRICE_UNIT.to_string(),
        details,
    });
}

pub fn enqueue_project_meter(
    project_id: Uuid,
    seconds: i64,
    recorded_at: DateTime<Utc>,
) {
    enqueue_platform_usage(
        project_id,
        PROJECT_METRIC,
        entity_seconds(1, seconds),
        recorded_at,
        json!({ "entity_count": 1 }),
    );
}

pub fn enqueue_unit_meter(
    project_id: Uuid,
    unit_count: i64,
    seconds: i64,
    recorded_at: DateTime<Utc>,
) {
    enqueue_platform_usage(
        project_id,
        UNIT_METRIC,
        entity_seconds(unit_count, seconds),
        recorded_at,
        json!({ "entity_count": unit_count }),
    );
}

pub fn enqueue_stage_runtime_meter(
    project_id: Uuid,
    stage_runtime_count: i64,
    seconds: i64,
    recorded_at: DateTime<Utc>,
) {
    enqueue_platform_usage(
        project_id,
        STAGE_RUNTIME_METRIC,
        entity_seconds(stage_runtime_count, seconds),
        recorded_at,
        json!({ "entity_count": stage_runtime_count }),
    );
}

pub fn enqueue_subscription_meter(
    project_id: Uuid,
    seconds: i64,
    recorded_at: DateTime<Utc>,
    user_id: Uuid,
    package_tier: i32,
) {
    enqueue_platform_usage(
        project_id,
        SUBSCRIPTION_METRIC,
        entity_seconds(1, seconds),
        recorded_at,
        json!({ "entity_count": 1, "user_id": user_id, "package_tier": package_tier }),
    );
}

fn entity_seconds(count: i64, seconds: i64) -> i64 {
    count.max(0).saturating_mul(seconds.max(0))
}
