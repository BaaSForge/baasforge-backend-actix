use crate::models::projects::ProjectRuntimeResponse;
use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::Serialize;

#[derive(Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::chat_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatSessionResponse {
    pub id: i64,
    pub project_id: i64,
    pub user_id: i64,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub stage_project_runtime_id: Option<i64>,
}

#[derive(Serialize)]
pub struct ChatSessionWithProjectRuntimeResponse {
    #[serde(flatten)]
    pub session: ChatSessionResponse,
    pub project_runtime: Option<ProjectRuntimeResponse>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::chat_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatSessionInsert {
    pub project_id: i64,
    pub user_id: i64,
    pub title: String,
    pub stage_project_runtime_id: Option<i64>,
}
