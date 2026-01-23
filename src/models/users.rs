use diesel::Insertable;
use diesel::Queryable;

#[derive(Debug, diesel_derive_enum::DbEnum)]
#[ExistingTypePath = "crate::schema::sql_types::PlatformType"]
#[DbValueStyle = "snake_case"]
pub enum PlatformType {
    Google,
    Github,
}

#[derive(Debug, Queryable)]
#[diesel(table_name = crate::schema::users)]
pub struct User {
    pub id: i64,
    #[allow(dead_code)]
    pub platform: PlatformType,
    #[allow(dead_code)]
    pub id_in_platform: String,
    #[allow(dead_code)]
    pub full_name: Option<String>,
}


#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::users)]
pub struct UserDTO {
    pub platform: PlatformType,
    pub id_in_platform: String,
    pub full_name: Option<String>,
}
