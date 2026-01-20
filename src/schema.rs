// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, Clone, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "platform_type"))]
    pub struct PlatformType;
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
