// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "chat_message_type"))]
    pub struct ChatMessageType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "chat_session_role_type"))]
    pub struct ChatSessionRoleType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "db_cluster_type"))]
    pub struct DbClusterType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "deployment_status_type"))]
    pub struct DeploymentStatusType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "framework_type"))]
    pub struct FrameworkType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "language_type"))]
    pub struct LanguageType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "platform_environment_type"))]
    pub struct PlatformEnvironmentType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "platform_type"))]
    pub struct PlatformType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "project_unit_type"))]
    pub struct ProjectUnitType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "server_cluster_type"))]
    pub struct ServerClusterType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "user_token_type"))]
    pub struct UserTokenType;
}

diesel::table! {
    app_settings (key) {
        key -> Text,
        value -> Nullable<Text>,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::ChatSessionRoleType;
    use super::sql_types::ChatMessageType;

    chat_messages (id) {
        id -> Int8,
        chat_session_id -> Int8,
        role -> ChatSessionRoleType,
        #[sql_name = "type"]
        type_ -> ChatMessageType,
        payload -> Nullable<Jsonb>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    chat_sessions (id) {
        id -> Int8,
        project_id -> Int8,
        user_id -> Int8,
        #[max_length = 150]
        title -> Varchar,
        created_at -> Timestamptz,
        stage_project_runtime_id -> Nullable<Int8>,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::PlatformEnvironmentType;
    use super::sql_types::DbClusterType;

    db_clusters (id) {
        id -> Int8,
        environment -> PlatformEnvironmentType,
        db_type -> DbClusterType,
        max_capacity -> Int4,
        metadata -> Nullable<Jsonb>,
    }
}

diesel::table! {
    packages (id) {
        id -> Int8,
        #[max_length = 255]
        title -> Varchar,
        #[max_length = 255]
        slug -> Varchar,
        description -> Nullable<Text>,
        price -> Float8,
        discount_percent -> Nullable<Int4>,
        is_trial -> Nullable<Bool>,
        is_active -> Bool,
        tier -> Int4,
    }
}

diesel::table! {
    payments (id) {
        id -> Int8,
        user_id -> Int8,
        amount -> Float8,
        package_id -> Int8,
        #[max_length = 255]
        package_title -> Nullable<Varchar>,
        #[max_length = 255]
        package_slug -> Nullable<Varchar>,
        package_price -> Nullable<Float8>,
        package_discount -> Nullable<Int4>,
        is_success -> Bool,
        gateway_metadata -> Nullable<Jsonb>,
        expires_at -> Nullable<Timestamptz>,
        package_is_tiral -> Bool,
        package_tier -> Int4,
    }
}

diesel::table! {
    project_releases (id) {
        id -> Int8,
        project_id -> Int8,
        #[max_length = 50]
        version -> Varchar,
        units_metadata -> Nullable<Jsonb>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::PlatformEnvironmentType;
    use super::sql_types::DeploymentStatusType;

    project_runtimes (id) {
        id -> Int8,
        project_id -> Int8,
        environment -> PlatformEnvironmentType,
        db_cluster_id -> Nullable<Int8>,
        db_cluster_deployment_metadata -> Nullable<Jsonb>,
        db_cluster_deployment_status -> Nullable<DeploymentStatusType>,
        db_cluster_deployment_is_healthy -> Nullable<Bool>,
        db_cluster_deployment_last_health -> Nullable<Timestamptz>,
        server_cluster_id -> Nullable<Int8>,
        server_cluster_deployment_metadata -> Nullable<Jsonb>,
        server_cluster_deployment_status -> Nullable<DeploymentStatusType>,
        server_cluster_deployment_is_healthy -> Nullable<Bool>,
        server_cluster_deployment_last_health -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::ProjectUnitType;

    project_units (id) {
        id -> Int8,
        project_id -> Int8,
        code -> Nullable<Text>,
        explanation -> Nullable<Text>,
        #[max_length = 50]
        version -> Varchar,
        parent_unit_id -> Nullable<Int8>,
        unit_type -> ProjectUnitType,
        is_draft -> Bool,
        chat_session_id -> Nullable<Int8>,
    }
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
    use super::sql_types::PlatformEnvironmentType;
    use super::sql_types::ServerClusterType;

    server_clusters (id) {
        id -> Int8,
        environment -> PlatformEnvironmentType,
        server_type -> ServerClusterType,
        max_capacity -> Int4,
        metadata -> Nullable<Jsonb>,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::UserTokenType;

    user_tokens (id) {
        id -> Int8,
        user_id -> Int8,
        token_type -> UserTokenType,
        token -> Text,
        expires_at -> Timestamptz,
        revoked -> Nullable<Bool>,
        created_at -> Timestamptz,
        is_signup_token -> Bool,
        refresh_token_id -> Nullable<Int8>,
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
        #[max_length = 255]
        email_in_platform -> Nullable<Varchar>,
    }
}

diesel::joinable!(chat_messages -> chat_sessions (chat_session_id));
diesel::joinable!(chat_sessions -> project_runtimes (stage_project_runtime_id));
diesel::joinable!(chat_sessions -> projects (project_id));
diesel::joinable!(chat_sessions -> users (user_id));
diesel::joinable!(payments -> packages (package_id));
diesel::joinable!(payments -> users (user_id));
diesel::joinable!(project_releases -> projects (project_id));
diesel::joinable!(project_runtimes -> db_clusters (db_cluster_id));
diesel::joinable!(project_runtimes -> projects (project_id));
diesel::joinable!(project_runtimes -> server_clusters (server_cluster_id));
diesel::joinable!(project_units -> chat_sessions (chat_session_id));
diesel::joinable!(project_units -> projects (project_id));
diesel::joinable!(projects -> users (owner_user_id));
diesel::joinable!(user_tokens -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(
    app_settings,
    chat_messages,
    chat_sessions,
    db_clusters,
    packages,
    payments,
    project_releases,
    project_runtimes,
    project_units,
    projects,
    server_clusters,
    user_tokens,
    users,
);
