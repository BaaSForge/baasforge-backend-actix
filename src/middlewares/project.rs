use crate::database;
use crate::libs::api_response::api_error;
use crate::middlewares::auth::AccessPayload;
use crate::routes::AppContext;
use crate::schema::projects::dsl::*;
use crate::services::auth::REFRESH_TOKEN_COOKIE_KEY;
use crate::services::documentation_share::{
    DOCUMENTATION_SHARE_SECRET_HEADER, is_documentation_share_key_valid,
};
use actix_web::Error;
use actix_web::body::BoxBody;
use actix_web::dev::ServiceResponse;
use actix_web::web;
use actix_web::{HttpMessage, HttpResponse, dev::ServiceRequest, middleware::Next};
use diesel::prelude::*;
use serde_json::json;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub enum ReleaseAccessAuth {
    Owner { user_id: Uuid },
    DocumentationShare,
}

fn extract_project_id_from_request(req: &ServiceRequest) -> Option<Uuid> {
    req.match_info()
        .get("id")
        .and_then(|s| Uuid::parse_str(s).ok())
}

fn has_refresh_token_cookie(req: &ServiceRequest) -> bool {
    req.cookie(REFRESH_TOKEN_COOKIE_KEY).is_some()
}

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
            let project_id = extract_project_id_from_request(&req);
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

            let project_opt = database::block_on_db(&app_context.pool, move |conn| {
                projects
                    .filter(id.eq(project_id))
                    .select((id, owner_user_id))
                    .first::<(Uuid, Uuid)>(conn)
                    .optional()
            })
            .await
            .map_err(|e| {
                log::error!("[projects_owner] DB/block error: {:?}", e);
                actix_web::error::ErrorServiceUnavailable("Database temporarily unavailable")
            })?;

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

pub fn project_release_access_guard_middleware_factory() -> impl Fn(
    ServiceRequest,
    Next<BoxBody>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<ServiceResponse<BoxBody>, Error>> + 'static>,
> + Clone {
    move |req: ServiceRequest, next: Next<BoxBody>| {
        Box::pin(async move {
            let path = req.path();
            let project_id = extract_project_id_from_request(&req);
            let Some(project_id) = project_id else {
                return Ok(req
                    .into_response(HttpResponse::Forbidden().json(api_error(
                        "Invalid project path used for `project_release_access_guard_middleware_factory`",
                        serde_json::json!({}),
                    )))
                    .map_into_boxed_body());
            };

            let app_context = req
                .app_data::<web::Data<AppContext>>()
                .ok_or_else(|| actix_web::error::ErrorInternalServerError("Missing AppContext"))?;

            let user_id = req.extensions().get::<AccessPayload>().map(|p| p.user_id);
            if let Some(user_id) = user_id {
                let is_owner = database::block_on_db(&app_context.pool, move |conn| {
                    projects
                        .filter(id.eq(project_id))
                        .select((id, owner_user_id))
                        .first::<(Uuid, Uuid)>(conn)
                        .optional()
                })
                .await
                .map_err(|e| {
                    log::error!("[project_release_access] DB/block error: {:?}", e);
                    actix_web::error::ErrorServiceUnavailable("Database temporarily unavailable")
                })?;

                match is_owner {
                    None => {
                        return Ok(req
                            .into_response(HttpResponse::NotFound().json(api_error(
                                "Project not found",
                                serde_json::json!({}),
                            )))
                            .map_into_boxed_body());
                    }
                    Some((_proj_id, owner_id_in_db)) if owner_id_in_db == user_id => {
                        req.extensions_mut()
                            .insert(ReleaseAccessAuth::Owner { user_id });
                        return next.call(req).await.map(|res| res.map_into_boxed_body());
                    }
                    Some(_) => {}
                }
            }

            let share_secret = req
                .headers()
                .get(DOCUMENTATION_SHARE_SECRET_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);

            let Some(share_secret) = share_secret else {
                let body = if has_refresh_token_cookie(&req) {
                    api_error(
                        "Authentication required: log in as project owner or provide a valid documentation share secret",
                        json!({"needs_refresh_token": true}),
                    )
                } else {
                    api_error(
                        "Authentication required: log in as project owner or provide a valid documentation share secret",
                        json!({}),
                    )
                };
                return Ok(req
                    .into_response(HttpResponse::Unauthorized().json(body))
                    .map_into_boxed_body());
            };

            let valid = database::block_on_db(&app_context.pool, move |conn| {
                is_documentation_share_key_valid(conn, project_id, &share_secret)
            })
            .await
            .map_err(|e| {
                log::error!("[project_release_access] DB/block error: {:?}", e);
                actix_web::error::ErrorServiceUnavailable("Database temporarily unavailable")
            })?;

            if !valid {
                return Ok(req
                    .into_response(HttpResponse::Forbidden().json(api_error(
                        "Invalid or expired documentation share secret",
                        serde_json::json!({}),
                    )))
                    .map_into_boxed_body());
            }

            req.extensions_mut()
                .insert(ReleaseAccessAuth::DocumentationShare);
            next.call(req).await.map(|res| res.map_into_boxed_body())
        })
    }
}
