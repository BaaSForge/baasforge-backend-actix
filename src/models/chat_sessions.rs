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
    pub live_production_release_id: Option<Uuid>,
}

/// Single session fetch: same row shape as list items’ session fields plus `read_only`.
#[derive(Serialize)]
pub struct ChatSessionDetailResponse {
    #[serde(flatten)]
    pub session: ChatSessionResponse,
    /// True when production’s live release no longer matches this session’s baseline and the live
    /// release was not created from this session—releases and unit edits are blocked.
    pub read_only: bool,
}

#[derive(Serialize)]
pub struct ChatSessionWithLastMessageResponse {
    #[serde(flatten)]
    pub session: ChatSessionResponse,
    /// True when production’s live release no longer matches this session’s baseline and the live
    /// release was not created from this session—releases and unit edits are blocked.
    pub read_only: bool,
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
    pub live_production_release_id: Option<Uuid>,
}
