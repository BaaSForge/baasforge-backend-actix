use crate::database;
use crate::libs::api_response::api_error;
use crate::middlewares::auth::AccessPayload;
use crate::middlewares::payment::PaymentPayload;
use crate::models::projects::{
    PlatformEnvironmentType, ProjectInsert, ProjectReleaseResponse, ProjectResponse,
    ProjectRuntimeInsert, ProjectRuntimeResponse, ProjectReleaseWithBaseline,
    ProjectRuntimeWithLiveReleaseApi, ProjectRuntimeWithLiveReleaseResponse, ProjectUnitResponse,
    ProjectUnitType,
};
use crate::services::releases::{
    attach_base_production_releases, attach_is_draft_flags_from_latest_production,
    project_release_id_for_revision_lane, ProjectReleaseRevisionLookup,
};
use crate::services::chat_worker::db::{
    list_distinct_context_slugs_from_units, load_project_units_by_keys,
    load_units_from_latest_production_metadata, modules_by_context_slugs_visible_tip_units,
};
use crate::routes::ws::events::{ProjectDeletedPayload, WsServerEvent};
use crate::routes::ws::{publish_event_to_channel, user_channel};
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::project_releases;
use crate::schema::project_runtimes;
use crate::schema::project_units;
use crate::schema::projects;
use crate::services::payment::{
    PaidFeature, most_complete_needed_package, user_missing_paid_features,
};
use crate::services::stage_release_from_controller::transaction_create_stage_release_from_controller;
use crate::services::stage_runtime_release::{
    StageRuntimeDeployDenied, StageRuntimeDetachDenied, apply_stage_release_deploy_to_runtime,
    apply_stage_runtime_detach_live_release,
};
use crate::routes::chat::handlers::{
    broadcast_production_live_release_changed, broadcast_stage_release_create_effects,
    http_response_for_stage_release_from_controller_error,
};
use crate::{libs::api_response::api_success, routes::AppContext};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::prelude::*;
use diesel::result::Error;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use serde_json::json;
use uuid::Uuid;

#[derive(Clone, Deserialize)]
pub struct ProjectRequest {
    pub name: String,
    pub slug: String,
}

#[derive(Deserialize)]
pub struct DeployReleaseBody {
    pub revision: f64,
    /// Chat session that owns the `project_releases` row (required: revision is not unique per project).
    pub chat_session_id: Uuid,
}

/// Body for [`handle_detach_stage_runtime_live_release`]: matches frontend union (revision | controller | detach-only).
#[derive(Deserialize)]
#[serde(untagged)]
pub enum DetachLiveReleaseBody {
    /// Variant A: deploy an existing stage release revision after clearing the previous live release.
    ByRevision {
        revision: f64,
        chat_session_id: Uuid,
    },
    /// Variant B: create/attach stage release from release-controller message scope (same as
    /// `POST .../release-controller-messages/{id}/stage-release`), bound to path `runtime_id`.
    ByController {
        chat_session_id: Uuid,
        release_controller_message_id: Uuid,
    },
    /// Detach only: `{}` — clear `live_release_id` (requires a release to be live).
    DetachOnly(DetachLiveReleaseBodyEmpty),
}

#[derive(Deserialize)]
pub struct DetachLiveReleaseBodyEmpty {}

async fn notify_stage_runtime_release_nats(
    context: &web::Data<AppContext>,
    project_id: Uuid,
    runtime_id: Uuid,
    deployed_release_id: Option<Uuid>,
    previous_release_id: Option<Uuid>,
    op_label: &'static str,
) {
    let subject = context.settings.runtime_nats_subject_release.as_str();
    let Some(ref nats) = context.runtime_nats_publisher else {
        tracing::warn!(
            target: "runtime_nats",
            op = op_label,
            subject = %subject,
            project_id = %project_id,
            runtime_id = %runtime_id,
            ?deployed_release_id,
            ?previous_release_id,
            "runtime NATS publisher not configured; stage runtime will not receive release via NATS"
        );
        return;
    };
    tracing::info!(
        target: "runtime_nats",
        op = op_label,
        subject = %subject,
        project_id = %project_id,
        runtime_id = %runtime_id,
        deployed_release_id = ?deployed_release_id,
        previous_release_id = ?previous_release_id,
        "notifying stage runtime via NATS (release deploy event for consumer)"
    );
    if let Err(e) = nats
        .publish_stage_runtime_release_state(
            subject,
            project_id,
            runtime_id,
            deployed_release_id,
            previous_release_id,
        )
        .await
    {
        tracing::warn!(
            target: "runtime_nats",
            op = op_label,
            subject = %subject,
            project_id = %project_id,
            runtime_id = %runtime_id,
            error = %e,
            "NATS publish failed after DB success; runtime may be stale"
        );
    } else {
        tracing::info!(
            target: "runtime_nats",
            op = op_label,
            subject = %subject,
            project_id = %project_id,
            runtime_id = %runtime_id,
            deployed_release_id = ?deployed_release_id,
            "NATS notify finished OK (message published; consumer should act)"
        );
    }
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

    let user_id = access_payload.user_id;
    let body = body.into_inner();

    let projects: Vec<ProjectResponse> = match database::block_on_db(&context.pool, move |conn| {
        projects::table
            .filter(projects::owner_user_id.eq(user_id))
            .select(ProjectResponse::as_select())
            .load::<ProjectResponse>(conn)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[handle_create_project] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    if projects.len() >= 3 {
        let missing_features = user_missing_paid_features(
            &payments_payload.active_payments,
            &[PaidFeature::LotsOfProjects],
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
        owner_user_id: user_id,
        slug: body.slug.clone(),
        name: body.name.clone(),
    };

    let project: ProjectResponse = match database::block_on_db(&context.pool, move |conn| {
        conn.transaction::<_, Error, _>(|conn| {
            let project = diesel::insert_into(projects::table)
                .values(&new_project)
                .returning(ProjectResponse::as_select())
                .get_result(conn)?;

            diesel::insert_into(project_runtimes::table)
                .values(&[
                    ProjectRuntimeInsert {
                        project_id: project.id,
                        environment: PlatformEnvironmentType::Stage,
                        name: Some("Stage 1".to_string()),
                        slug: Some("stage-1".to_string()),
                    },
                    ProjectRuntimeInsert {
                        project_id: project.id,
                        environment: PlatformEnvironmentType::Prod,
                        name: Some("Production".to_string()),
                        slug: Some("prod".to_string()),
                    },
                ])
                .execute(conn)?;

            Ok(project)
        })
    })
    .await
    {
        Ok(p) => p,
        Err(_) => {
            log::error!("[handle_create_project] Insert DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success("Project created successfully", json!(project)))
}

pub async fn handle_get_project(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get project");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    let project = match database::block_on_db(&context.pool, move |conn| {
        projects::table
            .filter(projects::owner_user_id.eq(user_id))
            .filter(projects::id.eq(project_id))
            .select(ProjectResponse::as_select())
            .first::<ProjectResponse>(conn)
            .optional()
    })
    .await
    {
        Ok(Some(proj)) => proj,
        Ok(None) => {
            return HttpResponse::NotFound().json(api_error("Project not found", json!({})));
        }
        Err(_) => {
            log::error!("[handle_get_project] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success("Project fetched successfully", json!(project)))
}

#[derive(Deserialize)]
pub struct ProjectPatchRequest {
    pub name: String,
}

pub async fn handle_patch_project(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    body: web::Json<ProjectPatchRequest>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to patch project");

    let project_id = path.into_inner();
    let name = body.name.clone();
    let user_id = access_payload.user_id;

    #[derive(Clone)]
    enum PatchOut {
        NameExists,
        NotFound,
        Updated(ProjectResponse),
    }
    impl PatchOut {
        fn into_response(self) -> HttpResponse {
            match self {
                PatchOut::NameExists => HttpResponse::BadRequest()
                    .json(api_error("Project name must be unique", json!({}))),
                PatchOut::NotFound => {
                    HttpResponse::NotFound().json(api_error("Project not found", json!({})))
                }
                PatchOut::Updated(proj) => {
                    HttpResponse::Ok().json(api_success("Project name updated", json!(proj)))
                }
            }
        }
    }

    match database::block_on_db(&context.pool, move |conn| {
        let exists = projects::table
            .filter(projects::owner_user_id.eq(user_id))
            .filter(projects::name.eq(&name))
            .filter(projects::id.ne(project_id))
            .select(projects::id)
            .first::<Uuid>(conn)
            .optional()?;
        if exists.is_some() {
            return Ok(PatchOut::NameExists);
        }
        let updated = diesel::update(
            projects::table
                .filter(projects::owner_user_id.eq(user_id))
                .filter(projects::id.eq(project_id)),
        )
        .set(projects::name.eq(&name))
        .returning(ProjectResponse::as_select())
        .get_result::<ProjectResponse>(conn)
        .optional()?;
        Ok(match updated {
            Some(p) => PatchOut::Updated(p),
            None => PatchOut::NotFound,
        })
    })
    .await
    {
        Ok(o) => o.into_response(),
        Err(_) => {
            log::error!("[handle_patch_project] DB/block error");
            HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})))
        }
    }
}

pub async fn handle_delete_project(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> HttpResponse {
    // TODO: add messages to queue to detach runtimes
    // TODO: create a saga which removes rows exactly when all side systems are detached successfully

    let extensions = req.extensions();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    let res = database::block_on_db(&context.pool, move |conn| {
        conn.transaction::<_, Error, _>(|conn| {
            let session_ids: Vec<Uuid> = chat_sessions::table
                .filter(chat_sessions::project_id.eq(project_id))
                .select(chat_sessions::id)
                .load(conn)?;
            if !session_ids.is_empty() {
                diesel::delete(
                    chat_messages::table.filter(chat_messages::chat_session_id.eq_any(session_ids)),
                )
                .execute(conn)?;
            }

            diesel::delete(project_units::table.filter(project_units::project_id.eq(project_id)))
                .execute(conn)?;

            diesel::delete(chat_sessions::table.filter(chat_sessions::project_id.eq(project_id)))
                .execute(conn)?;

            diesel::delete(
                project_runtimes::table.filter(project_runtimes::project_id.eq(project_id)),
            )
            .execute(conn)?;

            diesel::delete(
                project_releases::table.filter(project_releases::project_id.eq(project_id)),
            )
            .execute(conn)?;

            diesel::delete(
                projects::table
                    .filter(projects::id.eq(project_id))
                    .filter(projects::owner_user_id.eq(user_id)),
            )
            .execute(conn)?;

            Ok(())
        })
    })
    .await;

    let res: Result<(), Error> = match res {
        Ok(()) => Ok(()),
        Err(database::BlockingDbError::Query(e)) => Err(e),
        Err(_) => {
            log::error!("[handle_delete_project] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

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
            log::error!("[handle_delete_project] DB transaction error: {:?}", e);
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

    let user_id = access_payload.user_id;

    let projects = match database::block_on_db(&context.pool, move |conn| {
        projects::table
            .filter(projects::owner_user_id.eq(user_id))
            .select(ProjectResponse::as_select())
            .load::<ProjectResponse>(conn)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[list_projects] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Projects fetched successfully",
        json!(projects),
    ))
}

pub async fn handle_list_project_non_draft_units(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project units");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    let units = match database::block_on_db(&context.pool, move |conn| {
        let project_owned = projects::table
            .filter(projects::id.eq(project_id))
            .filter(projects::owner_user_id.eq(user_id))
            .select(projects::id)
            .first::<Uuid>(conn)
            .optional()?
            .is_some();
        if !project_owned {
            return Ok::<_, diesel::result::Error>(vec![]);
        }
        let rows = load_units_from_latest_production_metadata(conn, project_id)?;
        attach_is_draft_flags_from_latest_production(conn, project_id, rows)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[handle_list_project_units] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Project units fetched successfully",
        json!(units),
    ))
}

#[derive(Deserialize)]
pub struct ListProjectRuntimesQuery {
    pub stage: Option<bool>,
}

pub async fn handle_list_project_runtimes(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    query: web::Query<ListProjectRuntimesQuery>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project runtimes");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;
    let env_filter = query.stage.map(|is_stage| {
        if is_stage {
            PlatformEnvironmentType::Stage
        } else {
            PlatformEnvironmentType::Prod
        }
    });

    let runtimes = match database::block_on_db(&context.pool, move |conn| {
        let mut q = project_runtimes::table
            .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
            .left_join(
                project_releases::table.on(
                    project_runtimes::live_release_id.eq(project_releases::id.nullable()),
                ),
            )
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_runtimes::project_id.eq(project_id))
            .into_boxed();
        if let Some(env) = env_filter {
            q = q.filter(project_runtimes::environment.eq(env));
        }
        let rows: Vec<ProjectRuntimeWithLiveReleaseResponse> = q
            .select(ProjectRuntimeWithLiveReleaseResponse::as_select())
            .load::<ProjectRuntimeWithLiveReleaseResponse>(conn)?;
        let to_enrich: Vec<ProjectReleaseResponse> =
            rows.iter().filter_map(|r| r.live_release.clone()).collect();
        let by_id: HashMap<Uuid, ProjectReleaseWithBaseline> =
            if to_enrich.is_empty() {
                HashMap::new()
            } else {
                attach_base_production_releases(conn, project_id, to_enrich)?
                    .into_iter()
                    .map(|e| (e.release.id, e))
                    .collect()
            };
        Ok(rows
            .into_iter()
            .map(|r| {
                let live_release = r
                    .live_release
                    .as_ref()
                    .and_then(|lr| by_id.get(&lr.id).cloned());
                ProjectRuntimeWithLiveReleaseApi {
                    runtime: r.runtime,
                    live_release,
                }
            })
            .collect::<Vec<_>>())
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[handle_list_project_runtimes] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Project runtimes fetched successfully",
        json!(runtimes),
    ))
}

/// Detach live stage release and optionally rebind: by existing revision, by release-controller
/// message (same as chat stage-release), or `{}` for detach-only.
pub async fn handle_detach_stage_runtime_live_release(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<DetachLiveReleaseBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let (project_id, runtime_id) = path.into_inner();
    let user_id = access_payload.user_id;

    match body.into_inner() {
        DetachLiveReleaseBody::ByRevision {
            revision,
            chat_session_id,
        } => {
            let outcome = match database::block_on_db(&context.pool, move |conn| {
                apply_stage_release_deploy_to_runtime(
                    conn,
                    user_id,
                    project_id,
                    runtime_id,
                    revision,
                    chat_session_id,
                )
            })
            .await
            {
                Ok(o) => o,
                Err(_) => {
                    log::error!(
                        "[handle_detach_stage_runtime_live_release] DB/block error (by revision)"
                    );
                    return HttpResponse::ServiceUnavailable()
                        .json(api_error("Database temporarily unavailable", json!({})));
                }
            };

            match outcome {
                Ok((rt, nats_meta)) => {
                    // `previous_release_id` is None on first stage deploy (nothing was live before).
                    notify_stage_runtime_release_nats(
                        &context,
                        project_id,
                        runtime_id,
                        Some(nats_meta.deployed_release_id),
                        nats_meta.previous_release_id,
                        "handle_detach_stage_runtime_live_release",
                    )
                    .await;
                    HttpResponse::Ok().json(api_success(
                        "Stage release deployed on runtime",
                        json!({ "runtime": rt }),
                    ))
                }
                Err(StageRuntimeDeployDenied::NotFound) => HttpResponse::NotFound()
                    .json(api_error("Project runtime not found", json!({}))),
                Err(StageRuntimeDeployDenied::NotStage) => HttpResponse::BadRequest().json(
                    api_error(
                        "Only stage runtimes can receive a stage release deploy",
                        json!({}),
                    ),
                ),
                Err(StageRuntimeDeployDenied::NoSuchRelease) => HttpResponse::BadRequest().json(
                    api_error(
                        "No stage release with this revision exists for this project and chat session",
                        json!({}),
                    ),
                ),
                Err(StageRuntimeDeployDenied::AmbiguousRevision) => HttpResponse::BadRequest()
                    .json(api_error(
                        "Multiple stage release rows match this project, chat session, and revision",
                        json!({}),
                    )),
            }
        }
        DetachLiveReleaseBody::ByController {
            chat_session_id,
            release_controller_message_id,
        } => {
            let outcome = match database::block_on_db(&context.pool, move |conn| {
                transaction_create_stage_release_from_controller(
                    conn,
                    user_id,
                    project_id,
                    chat_session_id,
                    release_controller_message_id,
                    runtime_id,
                )
            })
            .await
            {
                Ok(o) => o,
                Err(_) => {
                    log::error!(
                        "[handle_detach_stage_runtime_live_release] DB/block error (by controller)"
                    );
                    return HttpResponse::ServiceUnavailable()
                        .json(api_error("Database temporarily unavailable", json!({})));
                }
            };

            match outcome {
                Ok((release, row, previous_stage_release_id)) => {
                    let release_for_broadcast = release.clone();
                    let release_with_base = match database::block_on_db(&context.pool, move |conn| {
                        attach_base_production_releases(conn, project_id, vec![release])
                    })
                    .await
                    {
                        Ok(mut v) => v.pop().expect("one release"),
                        Err(_) => {
                            log::error!(
                                "[handle_detach_stage_runtime_live_release] DB/block error (attach baseline)"
                            );
                            return HttpResponse::ServiceUnavailable()
                                .json(api_error("Database temporarily unavailable", json!({})));
                        }
                    };
                    broadcast_stage_release_create_effects(
                        &context,
                        project_id,
                        chat_session_id,
                        user_id,
                        runtime_id,
                        &release_for_broadcast,
                        row,
                        previous_stage_release_id,
                    )
                    .await;
                    HttpResponse::Ok().json(api_success(
                        "Stage release created successfully",
                        json!({ "release": release_with_base }),
                    ))
                }
                Err(e) => http_response_for_stage_release_from_controller_error(e),
            }
        }
        DetachLiveReleaseBody::DetachOnly(_) => {
            let outcome = match database::block_on_db(&context.pool, move |conn| {
                apply_stage_runtime_detach_live_release(conn, user_id, project_id, runtime_id)
            })
            .await
            {
                Ok(o) => o,
                Err(_) => {
                    log::error!(
                        "[handle_detach_stage_runtime_live_release] DB/block error (detach only)"
                    );
                    return HttpResponse::ServiceUnavailable()
                        .json(api_error("Database temporarily unavailable", json!({})));
                }
            };

            match outcome {
                Ok((rt, previous_release_id)) => {
                    tracing::info!(
                        target: "runtime_nats",
                        path = "POST .../detach-live-release",
                        body = "detach_only",
                        project_id = %project_id,
                        runtime_id = %runtime_id,
                        "detach-only succeeded in DB; publishing NATS event (clear live release for runtime)"
                    );
                    notify_stage_runtime_release_nats(
                        &context,
                        project_id,
                        runtime_id,
                        None,
                        previous_release_id,
                        "handle_detach_stage_runtime_live_release",
                    )
                    .await;
                    HttpResponse::Ok().json(api_success(
                        "Stage runtime detached from live release revision",
                        json!(rt),
                    ))
                }
                Err(StageRuntimeDetachDenied::NotFound) => HttpResponse::NotFound()
                    .json(api_error("Project runtime not found", json!({}))),
                Err(StageRuntimeDetachDenied::NotStage) => HttpResponse::BadRequest().json(
                    api_error(
                        "Only stage runtimes can be detached from a live release revision",
                        json!({}),
                    ),
                ),
                Err(StageRuntimeDetachDenied::NotConnected) => HttpResponse::BadRequest().json(
                    api_error(
                        "Stage runtime is not connected to a live release revision",
                        json!({}),
                    ),
                ),
            }
        }
    }
}

pub async fn handle_list_project_releases(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project releases");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    let releases = match database::block_on_db(&context.pool, move |conn| {
        let rows: Vec<ProjectReleaseResponse> = project_releases::table
            .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_releases::project_id.eq(project_id))
            .order(project_releases::revision.desc())
            .then_order_by(project_releases::created_at.desc())
            .select(ProjectReleaseResponse::as_select())
            .load::<ProjectReleaseResponse>(conn)?;
        attach_base_production_releases(conn, project_id, rows)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[handle_list_project_releases] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Project releases fetched successfully",
        json!(releases),
    ))
}

pub async fn handle_list_project_release_units(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project release units");

    let (project_id, release_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        let release: Option<ProjectReleaseResponse> = project_releases::table
            .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_releases::project_id.eq(project_id))
            .filter(project_releases::id.eq(release_id))
            .select(ProjectReleaseResponse::as_select())
            .first(conn)
            .optional()?;

        let Some(release) = release else {
            return Ok::<_, diesel::result::Error>(None);
        };

        let keys: Vec<(Uuid, String)> = release
            .units_metadata
            .as_ref()
            .map(|m| {
                m.new_unit_refs
                    .iter()
                    .map(|r| (r.chat_session_id, r.unit_id.clone()))
                    .collect()
            })
            .unwrap_or_default();

        if keys.is_empty() {
            return Ok(Some(vec![]));
        }

        let units = load_project_units_by_keys(conn, project_id, &keys)?;
        let units = attach_is_draft_flags_from_latest_production(conn, project_id, units)?;
        Ok(Some(units))
    })
    .await
    {
        Ok(o) => o,
        Err(database::BlockingDbError::Query(e)) => {
            log::error!(
                "[handle_list_project_release_units] Failed to load: {:?}",
                e
            );
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            log::error!("[handle_list_project_release_units] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let Some(units) = outcome else {
        return HttpResponse::NotFound().json(api_error("Project release not found", json!({})));
    };

    HttpResponse::Ok().json(api_success(
        "Project release units fetched successfully",
        json!(units),
    ))
}

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    20
}

/// `(context_slug, module_slug)` sent together; used to filter release units (see [`handle_list_project_release_units_all`]).
#[derive(Debug, Clone, Deserialize)]
pub struct ContextModulePair {
    pub context_slug: String,
    pub module_slug: String,
}

#[derive(Debug, Deserialize)]
pub struct ListReleaseUnitsAllBody {
    #[serde(default = "default_page")]
    pub page: u32,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    /// When non-empty, only these unit types are included.
    #[serde(default)]
    pub unit_types: Vec<ProjectUnitType>,
    /// When non-empty, only rows whose `context_slug` is in this list.
    #[serde(default)]
    pub context_slugs: Vec<String>,
    /// When non-empty, only rows whose `(context_slug, module_slug)` matches one of these pairs.
    #[serde(default)]
    pub context_module_pairs: Vec<ContextModulePair>,
}

/// Filters, sorts, and paginates in-memory. Non-empty filter dimensions are combined with AND.
fn filter_sort_paginate_release_units(
    mut units: Vec<ProjectUnitResponse>,
    unit_types: &[ProjectUnitType],
    context_slugs: &[String],
    context_module_pairs: &[ContextModulePair],
    page: u32,
    page_size: u32,
) -> (Vec<ProjectUnitResponse>, u64, u32, u32) {
    let type_set: Option<HashSet<ProjectUnitType>> = if unit_types.is_empty() {
        None
    } else {
        Some(unit_types.iter().copied().collect())
    };

    let ctx_set: Option<HashSet<String>> = {
        let v: Vec<String> = context_slugs
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if v.is_empty() {
            None
        } else {
            Some(v.into_iter().collect())
        }
    };

    let pair_set: Option<HashSet<(String, String)>> = {
        let v: Vec<(String, String)> = context_module_pairs
            .iter()
            .map(|p| (p.context_slug.trim().to_string(), p.module_slug.trim().to_string()))
            .filter(|(c, m)| !c.is_empty() && !m.is_empty())
            .collect();
        if v.is_empty() {
            None
        } else {
            Some(v.into_iter().collect())
        }
    };

    units.retain(|u| {
        if let Some(ref ts) = type_set {
            if !ts.contains(&u.unit_type) {
                return false;
            }
        }
        if let Some(ref cs) = ctx_set {
            if !cs.contains(u.context_slug.as_str()) {
                return false;
            }
        }
        if let Some(ref ps) = pair_set {
            if !ps.contains(&(u.context_slug.clone(), u.module_slug.clone())) {
                return false;
            }
        }
        true
    });

    units.sort_by(|a, b| {
        a.context_slug
            .cmp(&b.context_slug)
            .then_with(|| a.module_slug.cmp(&b.module_slug))
            .then_with(|| a.unit_name_slug.cmp(&b.unit_name_slug))
            .then_with(|| a.id.cmp(&b.id))
    });

    let total = units.len() as u64;
    let page = page.max(1);
    let page_size = page_size.clamp(1, 100);
    let offset = (page.saturating_sub(1) as usize).saturating_mul(page_size as usize);
    let page_units: Vec<_> = units.into_iter().skip(offset).take(page_size as usize).collect();
    (page_units, total, page, page_size)
}

/// All units in the release snapshot (`combinedUnitIDs`), with optional filters and pagination.
/// [`ListReleaseUnitsAllBody::context_module_pairs`] must send `context_slug` and `module_slug` together per row.
pub async fn handle_list_project_release_units_all(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<ListReleaseUnitsAllBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project release units (all)");

    let (project_id, release_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let page_req = body.page.max(1);
    let page_size_req = body.page_size.clamp(1, 100);
    let unit_types = body.unit_types.clone();
    let context_slugs = body.context_slugs.clone();
    let context_module_pairs = body.context_module_pairs.clone();

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        let release: Option<ProjectReleaseResponse> = project_releases::table
            .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_releases::project_id.eq(project_id))
            .filter(project_releases::id.eq(release_id))
            .select(ProjectReleaseResponse::as_select())
            .first(conn)
            .optional()?;

        let Some(release) = release else {
            return Ok::<_, diesel::result::Error>(None);
        };

        let keys: Vec<(Uuid, String)> = release
            .units_metadata
            .as_ref()
            .map(|m| {
                m.combined_unit_refs
                    .iter()
                    .map(|r| (r.chat_session_id, r.unit_id.clone()))
                    .collect()
            })
            .unwrap_or_default();

        if keys.is_empty() {
            let empty: Vec<ProjectUnitResponse> = vec![];
            return Ok(Some((empty, 0u64, page_req, page_size_req)));
        }

        let units = load_project_units_by_keys(conn, project_id, &keys)?;
        let units = attach_is_draft_flags_from_latest_production(conn, project_id, units)?;
        let (page_units, total, page, page_size) = filter_sort_paginate_release_units(
            units,
            &unit_types,
            &context_slugs,
            &context_module_pairs,
            page_req,
            page_size_req,
        );
        Ok(Some((page_units, total, page, page_size)))
    })
    .await
    {
        Ok(o) => o,
        Err(database::BlockingDbError::Query(e)) => {
            log::error!(
                "[handle_list_project_release_units_all] Failed to load: {:?}",
                e
            );
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            log::error!("[handle_list_project_release_units_all] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let Some((units, total, page, page_size)) = outcome else {
        return HttpResponse::NotFound().json(api_error("Project release not found", json!({})));
    };

    HttpResponse::Ok().json(api_success(
        "Project release units (all) fetched successfully",
        json!({
            "units": units,
            "total": total,
            "page": page,
            "page_size": page_size,
        }),
    ))
}

/// Distinct bounded-context slugs (`context_slug`) used by this release’s **new** units (`newUnitIDs`
/// metadata), same scope as [`handle_list_project_release_units`].
pub async fn handle_list_project_release_contexts(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project release contexts");

    let (project_id, release_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        let release: Option<ProjectReleaseResponse> = project_releases::table
            .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_releases::project_id.eq(project_id))
            .filter(project_releases::id.eq(release_id))
            .select(ProjectReleaseResponse::as_select())
            .first(conn)
            .optional()?;

        let Some(release) = release else {
            return Ok::<_, diesel::result::Error>(None);
        };

        let keys: Vec<(Uuid, String)> = release
            .units_metadata
            .as_ref()
            .map(|m| {
                m.new_unit_refs
                    .iter()
                    .map(|r| (r.chat_session_id, r.unit_id.clone()))
                    .collect()
            })
            .unwrap_or_default();

        if keys.is_empty() {
            return Ok(Some(Vec::new()));
        }

        let units = load_project_units_by_keys(conn, project_id, &keys)?;
        Ok(Some(list_distinct_context_slugs_from_units(&units)))
    })
    .await
    {
        Ok(o) => o,
        Err(database::BlockingDbError::Query(e)) => {
            log::error!(
                "[handle_list_project_release_contexts] Failed to load: {:?}",
                e
            );
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            log::error!("[handle_list_project_release_contexts] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let Some(context_slugs) = outcome else {
        return HttpResponse::NotFound().json(api_error("Project release not found", json!({})));
    };

    HttpResponse::Ok().json(api_success(
        "Project release contexts fetched successfully",
        json!({ "context_slugs": context_slugs }),
    ))
}

#[derive(Deserialize)]
pub struct ModulesByContextsBody {
    pub context_slugs: Vec<String>,
}

/// For each requested bounded-context slug, distinct `module_slug` values used by units in the
/// **latest production release** snapshot (same rules as the `list_modules_for_unit_contexts` tool).
pub async fn handle_list_project_modules_by_contexts(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    body: web::Json<ModulesByContextsBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list modules by contexts");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    let has_slugs = body
        .context_slugs
        .iter()
        .any(|s| !s.trim().is_empty());
    if !has_slugs {
        return HttpResponse::BadRequest().json(api_error(
            "context_slugs must contain at least one non-empty context slug",
            json!({}),
        ));
    }

    let slugs = body.context_slugs.clone();

    let by_context = match database::block_on_db(&context.pool, move |conn| {
        let project_ok: bool = projects::table
            .filter(projects::id.eq(project_id))
            .filter(projects::owner_user_id.eq(user_id))
            .select(projects::id)
            .first::<Uuid>(conn)
            .optional()?
            .is_some();

        if !project_ok {
            return Ok::<_, diesel::result::Error>(None);
        }

        let rows = modules_by_context_slugs_visible_tip_units(conn, project_id, &slugs)?;
        let by_context: Vec<ContextModulesApiRow> = rows
            .into_iter()
            .map(|(context_slug, module_slugs)| ContextModulesApiRow {
                context_slug,
                module_slugs,
            })
            .collect();
        Ok(Some(by_context))
    })
    .await
    {
        Ok(o) => o,
        Err(database::BlockingDbError::Query(e)) => {
            log::error!(
                "[handle_list_project_modules_by_contexts] Failed to load: {:?}",
                e
            );
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            log::error!("[handle_list_project_modules_by_contexts] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let Some(by_context) = by_context else {
        return HttpResponse::NotFound().json(api_error("Project not found", json!({})));
    };

    HttpResponse::Ok().json(api_success(
        "Modules by contexts fetched successfully",
        json!({ "by_context": by_context }),
    ))
}

#[derive(Serialize)]
struct ContextModulesApiRow {
    context_slug: String,
    module_slugs: Vec<String>,
}

/// Points a stage runtime at an existing stage release revision (no new `project_releases` row).
/// Publishes the same NATS event as creating a stage release from chat.
pub async fn handle_deploy_stage_release(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<DeployReleaseBody>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let (project_id, runtime_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let revision = body.revision;
    let chat_session_id = body.chat_session_id;

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        apply_stage_release_deploy_to_runtime(
            conn,
            user_id,
            project_id,
            runtime_id,
            revision,
            chat_session_id,
        )
    })
    .await
    {
        Ok(o) => o,
        Err(_) => {
            log::error!("[handle_deploy_stage_release] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        Ok((rt, nats_meta)) => {
            // `previous_release_id` is None on first stage deploy (nothing was live before).
            notify_stage_runtime_release_nats(
                &context,
                project_id,
                runtime_id,
                Some(nats_meta.deployed_release_id),
                nats_meta.previous_release_id,
                "handle_deploy_stage_release",
            )
            .await;
            HttpResponse::Ok().json(api_success(
                "Stage release deployed on runtime",
                json!({ "runtime": rt }),
            ))
        }
        Err(StageRuntimeDeployDenied::NotFound) => {
            HttpResponse::NotFound().json(api_error("Project runtime not found", json!({})))
        }
        Err(StageRuntimeDeployDenied::NotStage) => HttpResponse::BadRequest().json(api_error(
            "Only stage runtimes can receive a stage release deploy",
            json!({}),
        )),
        Err(StageRuntimeDeployDenied::NoSuchRelease) => HttpResponse::BadRequest().json(api_error(
            "No stage release with this revision exists for this project and chat session",
            json!({}),
        )),
        Err(StageRuntimeDeployDenied::AmbiguousRevision) => HttpResponse::BadRequest().json(
            api_error(
                "Multiple stage release rows match this project, chat session, and revision",
                json!({}),
            ),
        ),
    }
}

/// Points a production runtime at an existing production release revision (no new `project_releases` row).
/// Publishes the same NATS event as promoting to production from chat.
pub async fn handle_deploy_production_release(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<DeployReleaseBody>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");

    let (project_id, runtime_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let revision = body.revision;
    let chat_session_id = body.chat_session_id;

    enum Out {
        NotFound,
        NotProd,
        NoSuchRelease,
        AmbiguousRevision,
        Ok(ProjectRuntimeResponse, Uuid, Option<Uuid>),
    }

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        let rt: Option<ProjectRuntimeResponse> = project_runtimes::table
            .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_runtimes::project_id.eq(project_id))
            .filter(project_runtimes::id.eq(runtime_id))
            .filter(project_runtimes::environment.eq(PlatformEnvironmentType::Prod))
            .select(ProjectRuntimeResponse::as_select())
            .first(conn)
            .optional()?;

        let Some(rt) = rt else {
            return Ok(Out::NotFound);
        };

        if rt.environment != PlatformEnvironmentType::Prod {
            return Ok(Out::NotProd);
        }

        let deployed_release_id = match project_release_id_for_revision_lane(
            conn,
            project_id,
            revision,
            false,
            chat_session_id,
        )? {
            ProjectReleaseRevisionLookup::One(id) => id,
            ProjectReleaseRevisionLookup::NoRow => return Ok(Out::NoSuchRelease),
            ProjectReleaseRevisionLookup::Ambiguous => return Ok(Out::AmbiguousRevision),
        };

        let previous_release_id = rt.live_release_id;

        let updated = diesel::update(
            project_runtimes::table
                .filter(project_runtimes::id.eq(runtime_id))
                .filter(project_runtimes::project_id.eq(project_id)),
        )
        .set(project_runtimes::live_release_id.eq(Some(deployed_release_id)))
        .returning(ProjectRuntimeResponse::as_select())
        .get_result::<ProjectRuntimeResponse>(conn)?;

        Ok(Out::Ok(updated, deployed_release_id, previous_release_id))
    })
    .await
    {
        Ok(o) => o,
        Err(_) => {
            log::error!("[handle_deploy_production_release] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        Out::NotFound => {
            HttpResponse::NotFound().json(api_error("Project runtime not found", json!({})))
        }
        Out::NotProd => HttpResponse::BadRequest().json(api_error(
            "Only production runtimes can receive a production release deploy",
            json!({}),
        )),
        Out::NoSuchRelease => HttpResponse::BadRequest().json(api_error(
            "No production release with this revision exists for this project and chat session",
            json!({}),
        )),
        Out::AmbiguousRevision => HttpResponse::BadRequest().json(api_error(
            "Multiple production release rows match this project, chat session, and revision",
            json!({}),
        )),
        Out::Ok(rt, deployed_release_id, previous_release_id) => {
            if let Some(ref nats) = context.runtime_nats_publisher {
                let subject = context.settings.runtime_nats_subject_release.as_str();
                if let Err(e) = nats
                    .publish_prod_release(
                        subject,
                        project_id,
                        deployed_release_id,
                        previous_release_id,
                    )
                    .await
                {
                    tracing::warn!("[handle_deploy_production_release] NATS publish failed: {}", e);
                }
            } else {
                tracing::warn!(
                    target: "runtime_nats",
                    project_id = %project_id,
                    deployed_release_id = %deployed_release_id,
                    ?previous_release_id,
                    "runtime NATS publisher not configured; production deploy not announced to runtimes"
                );
            }
            broadcast_production_live_release_changed(&context, project_id).await;
            HttpResponse::Ok().json(api_success(
                "Production release deployed on runtime",
                json!({ "runtime": rt }),
            ))
        }
    }
}
