use crate::database;
use crate::libs::api_response::api_error;
use crate::middlewares::auth::AccessPayload;
use crate::middlewares::payment::PaymentPayload;
use crate::models::projects::{
    FrameworkType, LanguageType, PlatformEnvironmentType, ProjectInsert, ProjectReleaseResponse,
    ProjectResponse, ProjectRuntimeInsert, ProjectRuntimeResponse,
    ProjectRuntimeWithLiveReleaseResponse, ProjectUnitRow,
};
use crate::routes::ws::events::{ProjectDeletedPayload, WsServerEvent};
use crate::routes::ws::{publish_event_to_channel, user_channel};
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::project_releases;
use crate::schema::project_runtimes;
use crate::schema::project_units;
use crate::schema::projects;
use crate::services::chat_worker::db::project_units_with_latest_revision;
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
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Clone, Deserialize)]
pub struct ProjectRequest {
    pub framework: FrameworkType,
    pub name: String,
    pub slug: String,
    pub language: LanguageType,
}

#[derive(Deserialize)]
pub struct DeployReleaseBody {
    pub revision: f64,
}

/// Body for [`handle_detach_stage_runtime_live_release`]: matches frontend union (revision | controller | detach-only).
#[derive(Deserialize)]
#[serde(untagged)]
pub enum DetachLiveReleaseBody {
    /// Variant A: deploy an existing stage release revision after clearing the previous live release.
    ByRevision { revision: f64 },
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
        framework: body.framework.clone(),
        owner_user_id: user_id,
        slug: body.slug.clone(),
        language: body.language.clone(),
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
        let rows = project_units::table
            .inner_join(projects::table.on(project_units::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_units::project_id.eq(project_id))
            .filter(project_units::is_draft.eq(false))
            .select(ProjectUnitRow::as_select())
            .load::<ProjectUnitRow>(conn)?;
        project_units_with_latest_revision(conn, project_id, rows)
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
        q.select(ProjectRuntimeWithLiveReleaseResponse::as_select())
            .load::<ProjectRuntimeWithLiveReleaseResponse>(conn)
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
        DetachLiveReleaseBody::ByRevision { revision } => {
            let outcome = match database::block_on_db(&context.pool, move |conn| {
                apply_stage_release_deploy_to_runtime(
                    conn,
                    user_id,
                    project_id,
                    runtime_id,
                    revision,
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
                        "No stage release with this revision exists for the project",
                        json!({}),
                    ),
                ),
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
                    broadcast_stage_release_create_effects(
                        &context,
                        project_id,
                        chat_session_id,
                        user_id,
                        runtime_id,
                        &release,
                        row,
                        previous_stage_release_id,
                    )
                    .await;
                    HttpResponse::Ok().json(api_success(
                        "Stage release created successfully",
                        json!({ "release": release }),
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
        project_releases::table
            .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_releases::project_id.eq(project_id))
            .order(project_releases::created_at.desc())
            .select(ProjectReleaseResponse::as_select())
            .load::<ProjectReleaseResponse>(conn)
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
    path: web::Path<(Uuid, f64)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project release units");

    #[allow(unused)]
    let (project_id, _release_revision) = path.into_inner();
    let user_id = access_payload.user_id;

    // TODO: retrieve (chat_session_id, unit id string) pairs from release metadata (id includes revision)
    let units_keys_from_release_metadata: Vec<(Uuid, String)> = vec![];

    let units = match database::block_on_db(&context.pool, move |conn| {
        let keys = units_keys_from_release_metadata;
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
        if keys.is_empty() {
            return Ok(vec![]);
        }
        crate::services::chat_worker::db::load_project_units_by_keys(conn, project_id, &keys)
    })
    .await
    {
        Ok(u) => u,
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

    HttpResponse::Ok().json(api_success(
        "Project release units fetched successfully",
        json!(units),
    ))
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

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        apply_stage_release_deploy_to_runtime(conn, user_id, project_id, runtime_id, revision)
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
            "No stage release with this revision exists for the project",
            json!({}),
        )),
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

    enum Out {
        NotFound,
        NotProd,
        NoSuchRelease,
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

        let deployed_release_id = project_releases::table
            .filter(project_releases::project_id.eq(project_id))
            .filter(project_releases::revision.eq(revision))
            .filter(project_releases::is_stage.eq(false))
            .select(project_releases::id)
            .first::<Uuid>(conn)
            .optional()?;

        let Some(deployed_release_id) = deployed_release_id else {
            return Ok(Out::NoSuchRelease);
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
            "No production release with this revision exists for the project",
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
                    revision,
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
