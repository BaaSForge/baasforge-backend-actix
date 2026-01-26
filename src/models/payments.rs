use serde_json::Value;
use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::Serialize;

#[derive(Insertable)]
#[diesel(table_name = crate::schema::payments)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PaymentInsert {
    pub user_id: i64,
    pub amount: f64,
    pub package_id: i64,
    pub package_title: Option<String>,
    pub package_slug: Option<String>,
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
pub struct PaymentResponse {
    pub id: i64,
    pub user_id: i64,
    pub amount: f64,
    pub package_id: i64,
    pub package_title: Option<String>,
    pub package_slug: Option<String>,
    pub package_price: Option<f64>,
    pub package_discount: Option<i32>,
    pub package_is_tiral: bool,
    pub is_success: bool,
    pub expires_at: Option<DateTime<Utc>>,
}