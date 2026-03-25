use crate::models::chat_sessions::ChatSessionResponse;
use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::FrameworkType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum FrameworkType {
    Mvc,
}

#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::LanguageType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum LanguageType {
    Rust,
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
    Failed,
}

#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::PlatformEnvironmentType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum PlatformEnvironmentType {
    Stage,
    Prod,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, diesel_derive_enum::DbEnum, Serialize, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::ProjectUnitType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ProjectUnitType {
    Bootstrap,
    Action,
    Api,
    Command,
    Flow,
    Gateway,
    Projection,
    Query,
    Runtime,
    Schema,
    Timer,
}

impl ProjectUnitType {
    pub fn as_slug(&self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap",
            Self::Action => "action",
            Self::Api => "api",
            Self::Command => "command",
            Self::Flow => "flow",
            Self::Gateway => "gateway",
            Self::Projection => "projection",
            Self::Query => "query",
            Self::Runtime => "runtime",
            Self::Schema => "schema",
            Self::Timer => "timer",
        }
    }
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::project_units)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectUnitInsert {
    pub project_id: Uuid,
    pub chat_session_id: Uuid,
    pub id: String,
    pub revision: i32,
    pub code: Option<String>,
    pub explanation: Option<String>,
    pub is_draft: bool,
    pub unit_type: ProjectUnitType,
    pub is_deleted: bool,
    pub context_slug: String,
    pub module_slug: String,
    pub unit_name_slug: String,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectInsert {
    pub framework: FrameworkType,
    pub owner_user_id: Uuid,
    pub name: String,
    pub slug: String,
    pub language: LanguageType,
}

#[derive(Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectResponse {
    pub id: Uuid,
    pub framework: FrameworkType,
    pub name: String,
    pub slug: String,
    pub language: LanguageType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_units)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectUnitResponse {
    pub project_id: Uuid,
    pub chat_session_id: Uuid,
    pub id: String,
    pub revision: i32,
    pub code: Option<String>,
    pub explanation: Option<String>,
    pub is_draft: bool,
    pub unit_type: ProjectUnitType,
    pub is_deleted: bool,
    pub context_slug: String,
    pub module_slug: String,
    pub unit_name_slug: String,
}

#[derive(Serialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_runtimes)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub environment: PlatformEnvironmentType,
    pub db_cluster_id: Option<Uuid>,
    pub db_cluster_deployment_metadata: Option<Value>,
    pub db_cluster_deployment_status: Option<DeploymentStatusType>,
    pub db_cluster_deployment_is_healthy: Option<bool>,
    pub db_cluster_deployment_last_health: Option<DateTime<Utc>>,
    pub server_cluster_id: Option<Uuid>,
    pub server_cluster_deployment_metadata: Option<Value>,
    pub server_cluster_deployment_status: Option<DeploymentStatusType>,
    pub server_cluster_deployment_is_healthy: Option<bool>,
    pub server_cluster_deployment_last_health: Option<DateTime<Utc>>,
    pub live_release_version: Option<String>,
}

#[derive(Serialize)]
pub struct ProjectRuntimeWithConnectedChatResponse {
    #[serde(flatten)]
    pub runtime: ProjectRuntimeResponse,
    pub connected_chat_session: Option<ChatSessionResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_releases)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectReleaseResponse {
    pub id: Uuid,
    pub project_id: Uuid,
    pub version: String,
    pub notes: Option<String>,
    pub units_metadata: Option<Value>,
    pub created_at: DateTime<Utc>,
}
