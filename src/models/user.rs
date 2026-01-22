use diesel::Queryable;
use diesel::Insertable;

#[derive(Debug, diesel_derive_enum::DbEnum)]
#[ExistingTypePath = "crate::schema::sql_types::PlatformType"]
#[DbValueStyle = "snake_case"]
pub enum PlatformType {
  Google,
  Github,
}

#[derive(Debug, Queryable, Insertable)]
#[diesel(table_name = crate::schema::users)]
pub struct User {
  pub platform: PlatformType,
  pub id_in_platform: String,
  pub full_name: Option<String>,
}