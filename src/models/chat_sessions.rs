use chrono::{DateTime, Utc};
use diesel::Queryable;
use diesel::Selectable;
use serde::Serialize;
use diesel::Insertable;

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::chat_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatSessionResponse {
    pub id: i64,
    pub project_id: i64,
    pub user_id: i64,
    pub title: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::chat_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatSessionInsert {
    pub project_id: i64,
    pub user_id: i64,
    pub title: String,
}