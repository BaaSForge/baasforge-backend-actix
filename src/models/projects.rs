use crate::models::chat_sessions::ChatSessionResponse;
use diesel::Insertable;
use chrono::{DateTime, Utc};
use diesel::Queryable;
use diesel::Selectable;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::FrameworkType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum FrameworkType {
    Mvc
}

#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::LanguageType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum LanguageType {
    Rust
}


#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::DeploymentStatusType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStatusType {
    Deploying,
    Stopped,
    Pruned,
    Up,
    Failed
}

#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::PlatformEnvironmentType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum PlatformEnvironmentType {
    Stage,
    Prod
}

#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::ProjectUnitType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ProjectUnitType {
    Controller,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectInsert {
    pub framework: FrameworkType,
    pub owner_user_id: i64,
    pub name: String,
    pub slug: String,
    pub language: LanguageType,
}

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectResponse {
    pub id: i64,
    pub framework: FrameworkType,
    pub name: String,
    pub slug: String,
    pub language: LanguageType,
}

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_units)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectUnitResponse {
    pub id: i64,
    pub project_id: i64,
    pub code: Option<String>,
    pub explanation: Option<String>,
    pub version: String,
    pub parent_unit_id: Option<i64>,
    pub unit_type: ProjectUnitType,
    pub is_draft: bool,
    pub chat_session_id: Option<i64>,
}


#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_runtimes)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeResponse {
    pub id: i64,
    pub project_id: i64,
    pub environment: PlatformEnvironmentType,
    pub db_cluster_id: Option<i64>,
    pub db_cluster_deployment_metadata: Option<Value>,
    pub db_cluster_deployment_status: Option<DeploymentStatusType>,
    pub db_cluster_deployment_is_healthy: Option<bool>,
    pub db_cluster_deployment_last_health: Option<DateTime<Utc>>,
    pub server_cluster_id: Option<i64>,
    pub server_cluster_deployment_metadata: Option<Value>,
    pub server_cluster_deployment_status: Option<DeploymentStatusType>,
    pub server_cluster_deployment_is_healthy: Option<bool>,
    pub server_cluster_deployment_last_health: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct ProjectRuntimeWithConnectedChatResponse {
    #[serde(flatten)]
    pub runtime: ProjectRuntimeResponse,
    pub connected_chat_session: Option<ChatSessionResponse>,
}

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_releases)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectReleaseResponse {
    pub id: i64,
    pub project_id: i64,
    pub version: String,
    pub units_metadata: Option<Value>,
}