use serde_json::Value;
use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::Serialize;
use uuid::Uuid;

#[derive(Insertable)]
#[diesel(table_name = crate::schema::payments)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PaymentInsert {
    pub user_id: Uuid,
    pub amount: f64,
    pub package_id: Uuid,
    pub package_title: Option<String>,
    pub package_slug: Option<String>,
    pub package_tier: i32,
    pub package_price: Option<f64>,
    pub package_discount: Option<i32>,
    pub package_is_tiral: bool,
    pub is_success: bool,
    pub gateway_metadata: Option<Value>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::payments)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[derive(Debug)]
pub struct PaymentResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub amount: f64,
    pub package_id: Uuid,
    pub package_title: Option<String>,
    pub package_slug: Option<String>,
    pub package_tier: i32,
    pub package_price: Option<f64>,
    pub package_discount: Option<i32>,
    pub package_is_tiral: bool,
    pub is_success: bool,
    pub expires_at: Option<DateTime<Utc>>,
}