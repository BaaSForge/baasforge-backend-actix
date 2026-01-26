use crate::libs::api_response::api_error;
use crate::middlewares::auth::AccessPayload;
use crate::middlewares::payment::PaymentPayload;
use crate::models::projects::{FrameworkType, LanguageType, ProjectInsert, ProjectResponse};
use crate::schema::projects;
use crate::services::payment::{
    PaidFeature, most_complete_needed_package, user_missing_paid_features,
};
use crate::{libs::api_response::api_success, routes::AppContext};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::prelude::*;
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

    let projects: Vec<_> = projects::table
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
    
    if projects.iter().any(|p| p.slug == body.slug || p.name == body.name) {
        return HttpResponse::BadRequest().json(api_error(
            "Project slug and name must be unique",
            json!({}),
        ));
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

pub async fn handle_list_projects(
    context: web::Data<AppContext>,
    req: HttpRequest,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list projects");

    let mut conn = context.pool.get().expect("DB conn");

    let projects: Vec<_> = projects::table
        .filter(projects::owner_user_id.eq(access_payload.user_id))
        .select(ProjectResponse::as_select())
        .load::<ProjectResponse>(&mut conn)
        .unwrap_or_default();

    HttpResponse::Ok().json(api_success(
        "Projects fetched successfully",
        json!(projects),
    ))
}
