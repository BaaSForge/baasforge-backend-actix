use diesel::Insertable;
use diesel::Queryable;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, diesel_derive_enum::DbEnum, Serialize)]
#[ExistingTypePath = "crate::schema::sql_types::PlatformType"]
#[DbValueStyle = "snake_case"]
#[serde(rename_all = "snake_case")]
pub enum PlatformType {
    Google,
    Github,
}

#[derive(Debug, Queryable, Serialize)]
#[diesel(table_name = crate::schema::users)]
pub struct User {
    pub id: Uuid,
    #[allow(dead_code)]
    pub platform: PlatformType,
    #[allow(dead_code)]
    pub id_in_platform: String,
    #[allow(dead_code)]
    pub full_name: Option<String>,
    pub email_in_platform: Option<String>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::users)]
pub struct UserDTO {
    pub platform: PlatformType,
    pub id_in_platform: String,
    pub full_name: Option<String>,
    pub email_in_platform: Option<String>,
}
