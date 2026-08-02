use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Insertable)]
#[diesel(table_name = crate::schema::withdrawals)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct WithdrawalInsert {
    pub user_id: Uuid,
    pub amount: f64,
    pub gateway_metadata: Option<Value>,
}

#[derive(Serialize, Queryable, Selectable, Debug)]
#[diesel(table_name = crate::schema::withdrawals)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct WithdrawalResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub amount: f64,
    pub gateway_metadata: Option<Value>,
    pub created_at: DateTime<Utc>,
}
