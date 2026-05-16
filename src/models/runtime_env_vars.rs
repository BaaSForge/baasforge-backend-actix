use chrono::{DateTime, Utc};
use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Insertable)]
#[diesel(table_name = crate::schema::project_runtime_encryption_keys)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeEncryptionKeyInsert {
    pub runtime_id: Uuid,
    pub encryption_key: Vec<u8>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::project_runtime_encryption_keys)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeEncryptionKeyRow {
    pub runtime_id: Uuid,
    pub encryption_key: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::project_runtime_env_vars)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeEnvVarInsert {
    pub project_id: Uuid,
    pub runtime_id: Uuid,
    pub key: String,
    pub stored_value: String,
    pub is_encrypted: bool,
}

#[derive(Queryable, Selectable, Clone)]
#[diesel(table_name = crate::schema::project_runtime_env_vars)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectRuntimeEnvVarRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub runtime_id: Uuid,
    pub key: String,
    pub stored_value: String,
    pub is_encrypted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Key metadata for LLM tools (no secret values).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeEnvKeySummary {
    pub key: String,
    pub is_encrypted: bool,
}

#[derive(Serialize)]
pub struct RuntimeEnvVarGroupedResponse {
    pub key: String,
    #[serde(rename = "isEncrypted")]
    pub is_encrypted: bool,
    #[serde(flatten)]
    pub values: std::collections::HashMap<String, Option<String>>,
}

#[derive(Serialize)]
pub struct RuntimeEnvVarRevealResponse {
    pub key: String,
    pub runtime_id: Uuid,
    pub value: Option<String>,
    #[serde(rename = "isEncrypted")]
    pub is_encrypted: bool,
}
