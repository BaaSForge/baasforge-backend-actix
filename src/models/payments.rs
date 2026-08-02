use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, diesel_derive_enum::DbEnum, Serialize, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::BillingPeriodType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum BillingPeriod {
    Monthly,
    Annual,
}

impl Default for BillingPeriod {
    fn default() -> Self {
        BillingPeriod::Monthly
    }
}

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
    pub billing_period: BillingPeriod,
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
    pub billing_period: BillingPeriod,
    pub created_at: DateTime<Utc>,
    pub cancelled_at: Option<DateTime<Utc>>,
}
