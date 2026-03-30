// @generated automatically by Diesel CLI.

pub mod sql_types {
    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "chat_agent_type"))]
    pub struct ChatAgentType;

    #[derive(diesel::query_builder::QueryId, diesel::sql_types::SqlType)]
    #[diesel(postgres_type(name = "chat_message_status"))]
    pub struct ChatMessageStatus;

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
    use super::sql_types::ChatMessageStatus;
    use super::sql_types::ChatAgentType;

    chat_messages (id) {
        id -> Uuid,
        chat_session_id -> Uuid,
        role -> ChatSessionRoleType,
        #[sql_name = "type"]
        type_ -> ChatMessageType,
        payload -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        status -> ChatMessageStatus,
        invoking_message_id -> Nullable<Uuid>,
        agent -> Nullable<ChatAgentType>,
    }
}

diesel::table! {
    chat_sessions (id) {
        id -> Uuid,
        project_id -> Uuid,
        user_id -> Uuid,
        #[max_length = 150]
        title -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::PlatformEnvironmentType;
    use super::sql_types::DbClusterType;

    db_clusters (id) {
        id -> Uuid,
        environment -> PlatformEnvironmentType,
        db_type -> DbClusterType,
        max_capacity -> Int4,
        metadata -> Nullable<Jsonb>,
    }
}

diesel::table! {
    packages (id) {
        id -> Uuid,
        #[max_length = 255]
        title -> Varchar,
        #[max_length = 255]
        slug -> Varchar,
        tier -> Int4,
        description -> Nullable<Text>,
        price -> Float8,
        discount_percent -> Nullable<Int4>,
        is_trial -> Nullable<Bool>,
        is_active -> Bool,
    }
}

diesel::table! {
    payments (id) {
        id -> Uuid,
        user_id -> Uuid,
        amount -> Float8,
        package_id -> Uuid,
        #[max_length = 255]
        package_title -> Nullable<Varchar>,
        #[max_length = 255]
        package_slug -> Nullable<Varchar>,
        package_tier -> Int4,
        package_price -> Nullable<Float8>,
        package_discount -> Nullable<Int4>,
        is_success -> Bool,
        gateway_metadata -> Nullable<Jsonb>,
        expires_at -> Nullable<Timestamptz>,
        package_is_tiral -> Bool,
    }
}

diesel::table! {
    project_releases (id) {
        id -> Uuid,
        project_id -> Uuid,
        units_metadata -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        notes -> Nullable<Text>,
        chat_session_id -> Nullable<Uuid>,
        is_stage -> Bool,
        revision -> Int4,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::PlatformEnvironmentType;
    use super::sql_types::DeploymentStatusType;

    project_runtimes (id) {
        id -> Uuid,
        project_id -> Uuid,
        environment -> PlatformEnvironmentType,
        db_cluster_id -> Nullable<Uuid>,
        db_cluster_deployment_metadata -> Nullable<Jsonb>,
        db_cluster_deployment_status -> Nullable<DeploymentStatusType>,
        db_cluster_deployment_is_healthy -> Nullable<Bool>,
        db_cluster_deployment_last_health -> Nullable<Timestamptz>,
        server_cluster_id -> Nullable<Uuid>,
        server_cluster_deployment_metadata -> Nullable<Jsonb>,
        server_cluster_deployment_status -> Nullable<DeploymentStatusType>,
        server_cluster_deployment_is_healthy -> Nullable<Bool>,
        server_cluster_deployment_last_health -> Nullable<Timestamptz>,
        #[max_length = 255]
        name -> Nullable<Varchar>,
        #[max_length = 100]
        slug -> Nullable<Varchar>,
        live_release_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::ProjectUnitType;

    project_units (project_id, chat_session_id, id, revision) {
        project_id -> Uuid,
        code -> Nullable<Text>,
        explanation -> Nullable<Text>,
        is_draft -> Bool,
        chat_session_id -> Uuid,
        unit_type -> ProjectUnitType,
        is_deleted -> Bool,
        #[max_length = 255]
        context_slug -> Varchar,
        #[max_length = 255]
        module_slug -> Varchar,
        #[max_length = 255]
        unit_name_slug -> Varchar,
        id -> Text,
        revision -> Int4,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::FrameworkType;
    use super::sql_types::LanguageType;

    projects (id) {
        id -> Uuid,
        framework -> FrameworkType,
        owner_user_id -> Uuid,
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
        id -> Uuid,
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
        id -> Uuid,
        user_id -> Uuid,
        token_type -> UserTokenType,
        token -> Text,
        expires_at -> Timestamptz,
        revoked -> Nullable<Bool>,
        created_at -> Timestamptz,
        is_signup_token -> Bool,
        refresh_token_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use super::sql_types::PlatformType;

    users (id) {
        id -> Uuid,
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
diesel::joinable!(chat_sessions -> projects (project_id));
diesel::joinable!(chat_sessions -> users (user_id));
diesel::joinable!(payments -> packages (package_id));
diesel::joinable!(payments -> users (user_id));
diesel::joinable!(project_releases -> chat_sessions (chat_session_id));
diesel::joinable!(project_releases -> projects (project_id));
diesel::joinable!(project_runtimes -> db_clusters (db_cluster_id));
diesel::joinable!(project_runtimes -> project_releases (live_release_id));
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
