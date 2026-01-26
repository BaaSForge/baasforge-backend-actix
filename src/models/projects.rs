use diesel::Insertable;
use diesel::Queryable;
use diesel::Selectable;
use serde::Deserialize;
use serde::Serialize;

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