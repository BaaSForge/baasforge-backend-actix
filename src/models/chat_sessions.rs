use crate::models::chat_messages::ChatMessageResponse;
use crate::models::projects::{ProjectReleaseResponse, ProjectUnitResponse};
use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::chat_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatSessionResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct ChatSessionWithLastMessageResponse {
    #[serde(flatten)]
    pub session: ChatSessionResponse,
    pub last_message: Option<ChatMessageResponse>,
    pub owned_units: Vec<ProjectUnitResponse>,
    pub owned_releases: Vec<ProjectReleaseResponse>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::chat_sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ChatSessionInsert {
    pub project_id: Uuid,
    pub user_id: Uuid,
    pub title: String,
}
