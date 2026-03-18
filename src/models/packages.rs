use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::Serialize;
use uuid::Uuid;

#[derive(Serialize, Queryable, Selectable, Insertable)]
#[diesel(table_name = crate::schema::packages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Package {
    pub id: Uuid,
    pub title: String,
    pub slug: String,
    pub tier: i32,
    pub description: Option<String>,
    pub price: f64,
    pub discount_percent: Option<i32>,
    pub is_trial: Option<bool>,
    pub is_active: bool,
}
