use crate::libs::api_response::api_error;
use crate::middlewares::auth::AccessPayload;
use crate::routes::AppContext;
use crate::schema::projects::dsl::*;
use actix_web::Error;
use actix_web::body::BoxBody;
use actix_web::dev::ServiceResponse;
use actix_web::web;
use actix_web::{HttpMessage, HttpResponse, dev::ServiceRequest, middleware::Next};
use diesel::prelude::*;
use regex::Regex;

pub fn projects_owner_guard_middleware_factory() -> impl Fn(
    ServiceRequest,
    Next<BoxBody>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<ServiceResponse<BoxBody>, Error>> + 'static>,
> + Clone {
    move |req: ServiceRequest, next: Next<BoxBody>| {
        Box::pin(async move {
            let user_id = {
                let extensions = req.extensions();

                let access_payload = extensions
                    .get::<AccessPayload>()
                    .expect("AccessPayload must exist to check project ownership");

                access_payload.user_id
            };

            let path = req.path();
            let re = Regex::new(r".*/platform/projects/(\d+)(/.*)?$").unwrap();
            let captures = re.captures(path);
            let project_id = if let Some(caps) = captures {
                caps.get(1).unwrap().as_str().parse::<i64>().ok()
            } else {
                None
            };

            if project_id.is_none() {
                return Ok(req
                    .into_response(HttpResponse::Forbidden().json(api_error(
                        "Invalid project path used for `projects_owner_guard_middleware_factory`",
                        serde_json::json!({}),
                    )))
                    .map_into_boxed_body());
            }

            let project_id = project_id.unwrap();

            let app_context = req
                .app_data::<web::Data<AppContext>>()
                .ok_or_else(|| actix_web::error::ErrorInternalServerError("Missing AppContext"))?;

            let mut conn = app_context.pool.get().expect("Failed to get DB connection");

            let project_opt = projects
                .filter(id.eq(project_id))
                .select((id, owner_user_id))
                .first::<(i64, i64)>(&mut conn)
                .optional()
                .expect("DB query failed");

            match project_opt {
                None => {
                    log::warn!(
                        "[projects_owner] Forbidden: path={} reason=project_not_found project_id={}",
                        path,
                        project_id
                    );
                    return Ok(req
                        .into_response(
                            HttpResponse::NotFound()
                                .json(api_error("Project not found", serde_json::json!({}))),
                        )
                        .map_into_boxed_body());
                }
                Some((_proj_id, owner_id_in_db)) if owner_id_in_db != user_id => {
                    log::warn!(
                        "[projects_owner] Forbidden: path={} reason=not_owner project_id={} user_id={} owner_id={}",
                        path,
                        project_id,
                        user_id,
                        owner_id_in_db
                    );
                    return Ok(req
                        .into_response(HttpResponse::Forbidden().json(api_error(
                            "You are not the owner of this project",
                            serde_json::json!({}),
                        )))
                        .map_into_boxed_body());
                }
                _ => {}
            }

            next.call(req).await.map(|res| res.map_into_boxed_body())
        })
    }
}
