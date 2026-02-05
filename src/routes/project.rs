use crate::libs::api_response::api_error;
use crate::middlewares::auth::AccessPayload;
use crate::middlewares::payment::PaymentPayload;
use crate::models::projects::{
    FrameworkType, LanguageType, ProjectUnitResponse, ProjectInsert, ProjectReleaseResponse,
    ProjectResponse, ProjectRuntimeResponse,
};
use crate::schema::project_units;
use crate::schema::project_releases;
use crate::schema::project_runtimes;
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
            &payments_payload.db_payments,
            &vec![PaidFeature::LotsOfProjects],
        );
        let most_complete_needed_package =
            most_complete_needed_package(&missing_features).unwrap_or("unknown");

        if !missing_features.is_empty() {
            return HttpResponse::Forbidden().json(api_success(
                &format!(
                    "You don't have enough purchased package to have more than 3 projects, you need package {} to do this action",
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
        diesel::delete(project_runtimes::table.filter(project_runtimes::project_id.eq(project_id)))
            .execute(conn)?;

        diesel::delete(
            project_units::table.filter(project_units::project_id.eq(project_id)),
        )
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
        Ok(_) => HttpResponse::Ok().json(api_success(
            "Project and it's runtimes, units and releases has been deleted successfully",
            json!({ "project_id": project_id }),
        )),
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

pub async fn handle_list_project_units(
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
        .select(ProjectUnitResponse::as_select())
        .load::<ProjectUnitResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Project units fetched successfully",
        json!(units),
    ))
}

pub async fn handle_list_project_runtimes(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<i64>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project runtimes");

    let project_id = path.into_inner();

    let mut conn = context.pool.get().expect("DB conn");

    let runtimes = project_runtimes::table
        .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .filter(project_runtimes::project_id.eq(project_id))
        .select(ProjectRuntimeResponse::as_select())
        .load::<ProjectRuntimeResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Project runtimes fetched successfully",
        json!(runtimes),
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
        .select(ProjectReleaseResponse::as_select())
        .load::<ProjectReleaseResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Project releases fetched successfully",
        json!(releases),
    ))
}