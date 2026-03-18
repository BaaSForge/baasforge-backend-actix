use diesel::Queryable;
use diesel::Selectable;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::models::projects::PlatformEnvironmentType;

#[derive(Debug, diesel_derive_enum::DbEnum, Serialize, Clone, Deserialize)]
#[ExistingTypePath = "crate::schema::sql_types::ServerClusterType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum ServerClusterType {
    K8s,
}

#[derive(Queryable, Selectable, Serialize)]
#[diesel(table_name = crate::schema::server_clusters)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ServerCluster {
    pub environment: PlatformEnvironmentType,
    pub server_type: ServerClusterType,
    pub max_capacity: i32,
    pub metadata: Option<Value>,
}
