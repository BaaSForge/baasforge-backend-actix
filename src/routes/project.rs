use crate::libs::api_response::api_error;
use crate::middlewares::auth::AccessPayload;
use crate::middlewares::payment::PaymentPayload;
use crate::models::chat_sessions::ChatSessionResponse;
use crate::routes::ws::events::{ProjectDeletedPayload, WsServerEvent};
use crate::routes::ws::{publish_event_to_channel, user_channel};
use crate::models::projects::{
    FrameworkType, LanguageType, PlatformEnvironmentType, ProjectInsert, ProjectReleaseResponse,
    ProjectResponse, ProjectRuntimeResponse, ProjectRuntimeWithConnectedChatResponse,
    ProjectUnitResponse,
};
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::project_releases;
use crate::schema::project_runtimes;
use crate::schema::project_units;
use crate::schema::projects;
use crate::services::payment::{
    PaidFeature, most_complete_needed_package, user_missing_paid_features,
};
use crate::{libs::api_response::api_success, routes::AppContext};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::prelude::*;
use diesel::result::Error;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

#[derive(Clone, Deserialize)]
pub struct ProjectRequest {
    pub framework: FrameworkType,
    pub name: String,
    pub slug: String,
    pub language: LanguageType,
}

pub async fn handle_create_project(
    context: web::Data<AppContext>,
    req: HttpRequest,
    body: web::Json<ProjectRequest>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let payments_payload = extensions
        .get::<PaymentPayload>()
        .expect("PaymentPayload must exist");

    let mut conn = context.pool.get().expect("DB conn");

    let projects = projects::table
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .select(ProjectResponse::as_select())
        .load::<ProjectResponse>(&mut conn)
        .unwrap_or_default();

    if projects.len() >= 3 {
        let missing_features = user_missing_paid_features(
            &payments_payload.active_payments,
            &vec![PaidFeature::LotsOfProjects],
        );

        let most_complete_needed_package =
            most_complete_needed_package(&missing_features).unwrap_or("unknown");

        if !missing_features.is_empty() {
            return HttpResponse::Forbidden().json(api_success(
                &format!(
                    "You don't have enough purchased package to have more than 3 projects, you need at least package {} to do this action",
                    most_complete_needed_package
                ),
                json!({
                    "missing_features": missing_features,
                    "most_complete_needed_package": most_complete_needed_package,
                }),
            ));
        }
    }

    if projects
        .iter()
        .any(|p| p.slug == body.slug || p.name == body.name)
    {
        return HttpResponse::BadRequest()
            .json(api_error("Project slug and name must be unique", json!({})));
    }

    let new_project = ProjectInsert {
        framework: body.framework.clone(),
        owner_user_id: access_payload.user_id,
        slug: body.slug.clone(),
        language: body.language.clone(),
        name: body.name.clone(),
    };

    let project: ProjectResponse = diesel::insert_into(projects::table)
        .values(&new_project)
        .returning(ProjectResponse::as_select())
        .get_result(&mut conn)
        .expect("Insert failed");

    HttpResponse::Ok().json(api_success("Project created successfully", json!(project)))
}

pub async fn handle_get_project(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get project");

    let project_id = path.into_inner();

    let mut conn = context.pool.get().expect("DB conn");

    let project = projects::table
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(projects::id.eq(project_id))
        .select(ProjectResponse::as_select())
        .first::<ProjectResponse>(&mut conn)
        .optional()
        .expect("DB query failed");

    match project {
        Some(proj) => {
            HttpResponse::Ok().json(api_success("Project fetched successfully", json!(proj)))
        }
        None => HttpResponse::NotFound().json(api_error("Project not found", json!({}))),
    }
}

#[derive(Deserialize)]
pub struct ProjectPatchRequest {
    pub name: String,
}

pub async fn handle_patch_project(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
    body: web::Json<ProjectPatchRequest>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to patch project");

    let project_id = path.into_inner();
    let mut conn = context.pool.get().expect("DB conn");

    let exists = projects::table
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(projects::name.eq(&body.name))
        .filter(projects::id.ne(project_id))
        .select(projects::id)
        .first::<i64>(&mut conn)
        .optional()
        .expect("DB query failed");

    if exists.is_some() {
        return HttpResponse::BadRequest()
            .json(api_error("Project name must be unique", json!({})));
    }

    let updated_project = diesel::update(
        projects::table
            .filter(projects::owner_user_id.eq(access_payload.user_id))
            .filter(projects::id.eq(project_id)),
    )
    .set(projects::name.eq(&body.name))
    .returning(ProjectResponse::as_select())
    .get_result::<ProjectResponse>(&mut conn)
    .optional()
    .expect("DB update failed");

    match updated_project {
        Some(proj) => HttpResponse::Ok().json(api_success("Project name updated", json!(proj))),
        None => HttpResponse::NotFound().json(api_error("Project not found", json!({}))),
    }
}

pub async fn handle_delete_project(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
) -> HttpResponse {
    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let project_id = path.into_inner();

    let mut conn = context.pool.get().expect("DB conn");

    // TODO: add messages to queue to detach runtimes

    // TODO: create a saga which removes rows exactly when all side systems are detached successfully
    let res = conn.transaction::<_, Error, _>(|conn| {
        let session_ids: Vec<i64> = chat_sessions::table
            .filter(chat_sessions::project_id.eq(project_id))
            .select(chat_sessions::id)
            .load(conn)?;
        if !session_ids.is_empty() {
            diesel::delete(
                chat_messages::table
                    .filter(chat_messages::chat_session_id.eq_any(session_ids)),
            )
            .execute(conn)?;
        }

        diesel::delete(project_units::table.filter(project_units::project_id.eq(project_id)))
            .execute(conn)?;

        diesel::delete(chat_sessions::table.filter(chat_sessions::project_id.eq(project_id)))
            .execute(conn)?;

        diesel::delete(project_runtimes::table.filter(project_runtimes::project_id.eq(project_id)))
            .execute(conn)?;

        diesel::delete(project_releases::table.filter(project_releases::project_id.eq(project_id)))
            .execute(conn)?;

        diesel::delete(
            projects::table
                .filter(projects::id.eq(project_id))
                .filter(projects::owner_user_id.eq(access_payload.user_id)),
        )
        .execute(conn)?;

        Ok(())
    });

    match res {
        Ok(_) => {
            if let Some(ref redis) = context.redis_conn {
                let channel = user_channel(access_payload.user_id);
                let event = WsServerEvent::ProjectDeleted(ProjectDeletedPayload {
                    project_id: project_id.to_string(),
                });
                let mut guard = redis.lock().await;
                if let Err(e) = publish_event_to_channel(&mut *guard, &channel, &event).await {
                    log::warn!("[ws] Failed to broadcast project deleted to Redis: {}", e);
                }
            }
            HttpResponse::Ok().json(api_success(
                "Project and its runtimes, units, releases, chat sessions and messages have been deleted successfully",
                json!({ "project_id": project_id }),
            ))
        }
        Err(Error::NotFound) => {
            HttpResponse::NotFound().json(api_error("Project not found", json!({})))
        }
        Err(e) => {
            eprintln!("DB transaction error: {:?}", e);
            HttpResponse::InternalServerError().finish()
        }
    }
}

pub async fn handle_list_projects(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list projects");

    let mut conn = context.pool.get().expect("DB conn");

    let projects = projects::table
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .select(ProjectResponse::as_select())
        .load::<ProjectResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Projects fetched successfully",
        json!(projects),
    ))
}

pub async fn handle_list_project_non_draft_units(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project units");

    let project_id = path.into_inner();

    let mut conn = context.pool.get().expect("DB conn");

    let units = project_units::table
        .inner_join(projects::table.on(project_units::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(project_units::project_id.eq(project_id))
        .filter(project_units::is_draft.eq(false))
        .select(ProjectUnitResponse::as_select())
        .load::<ProjectUnitResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Project units fetched successfully",
        json!(units),
    ))
}

#[derive(Deserialize)]
pub struct ListProjectRuntimesQuery {
    pub stage: Option<bool>,
}

pub async fn handle_list_project_runtimes_with_connected_chat_sessions(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
    query: web::Query<ListProjectRuntimesQuery>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project runtimes");

    let project_id = path.into_inner();

    let mut conn = context.pool.get().expect("DB conn");

    let env_filter = query.stage.map(|is_stage| {
        if is_stage {
            PlatformEnvironmentType::Stage
        } else {
            PlatformEnvironmentType::Prod
        }
    });

    let mut query = project_runtimes::table
        .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(project_runtimes::project_id.eq(project_id))
        .into_boxed();

    if let Some(env) = env_filter {
        query = query.filter(project_runtimes::environment.eq(env));
    }

    let runtimes: Vec<ProjectRuntimeResponse> = query
        .select(ProjectRuntimeResponse::as_select())
        .load::<ProjectRuntimeResponse>(&mut conn)
        .unwrap_or_default();

    let runtime_ids: Vec<i64> = runtimes.iter().map(|r| r.id).collect();
    let connected_sessions: HashMap<i64, ChatSessionResponse> = if runtime_ids.is_empty() {
        HashMap::new()
    } else {
        chat_sessions::table
            .inner_join(projects::table.on(chat_sessions::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(access_payload.user_id))
            .filter(chat_sessions::project_id.eq(project_id))
            .filter(chat_sessions::stage_project_runtime_id.eq_any(&runtime_ids))
            .select(ChatSessionResponse::as_select())
            .load::<ChatSessionResponse>(&mut conn)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| s.stage_project_runtime_id.map(|rid| (rid, s)))
            .collect()
    };

    let runtimes_with_chat: Vec<ProjectRuntimeWithConnectedChatResponse> = runtimes
        .into_iter()
        .map(|runtime| ProjectRuntimeWithConnectedChatResponse {
            connected_chat_session: connected_sessions.get(&runtime.id).cloned(),
            runtime,
        })
        .collect();

    HttpResponse::Ok().json(api_success(
        "Project runtimes fetched successfully",
        json!(runtimes_with_chat),
    ))
}

pub async fn handle_list_project_releases(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project releases");

    let project_id = path.into_inner();

    let mut conn = context.pool.get().expect("DB conn");

    let releases = project_releases::table
        .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(project_releases::project_id.eq(project_id))
        .order(project_releases::created_at.desc())
        .select(ProjectReleaseResponse::as_select())
        .load::<ProjectReleaseResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Project releases fetched successfully",
        json!(releases),
    ))
}

pub async fn handle_list_project_release_units(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(i64, String)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project release units");

    #[allow(unused)]
    let (project_id, release_version) = path.into_inner();

    let mut conn = context.pool.get().expect("DB conn");

    // TODO: retrieve units ids from release metadata
    let units_ids_from_release_metadata = [1, 2, 3, 4];

    let units = match project_units::table
        .inner_join(projects::table.on(project_units::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(project_units::project_id.eq(project_id))
        .filter(project_units::id.eq_any(&units_ids_from_release_metadata))
        .select(ProjectUnitResponse::as_select())
        .load::<ProjectUnitResponse>(&mut conn)
    {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Failed to load project release units: {:?}", e);
            return HttpResponse::InternalServerError()
                .json(api_error(&e.to_string(), json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Project release units fetched successfully",
        json!(units),
    ))
}