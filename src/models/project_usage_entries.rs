use chrono::{DateTime, Utc};
use diesel::Queryable;
use diesel::Selectable;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_usage_entries)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[derive(Debug)]
pub struct ProjectUsageEntryResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub kind: String,
    pub metric: String,
    pub quantity: i64,
    pub price_unit: String,
    pub details: Value,
}
