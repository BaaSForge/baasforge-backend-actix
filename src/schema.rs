// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "framework_type"))]
    pub struct FrameworkType;

    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "language_type"))]
    pub struct LanguageType;

    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "platform_type"))]
    pub struct PlatformType;
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::FrameworkType;
    use super::sql_types::LanguageType;

    projects (id) {
        id -> Int8,
        framework -> FrameworkType,
        owner_user_id -> Int8,
        #[max_length = 255]
        name -> Varchar,
        #[max_length = 255]
        slug -> Varchar,
        language -> LanguageType,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::PlatformType;

    users (id) {
        id -> Int8,
        platform -> PlatformType,
        #[max_length = 255]
        id_in_platform -> Varchar,
        #[max_length = 255]
        full_name -> Nullable<Varchar>,
    }
}

diesel::joinable!(projects -> users (owner_user_id));

diesel::allow_tables_to_appear_in_same_query!(projects, users,);
