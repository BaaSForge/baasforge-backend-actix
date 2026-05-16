use crate::database;
use crate::libs::api_response::api_error;
use crate::libs::unit_slug::{
    normalize_full_unit_id_arg, parse_unit_path, validate_full_unit_id_for_tool,
};
use crate::middlewares::auth::AccessPayload;
use crate::middlewares::payment::PaymentPayload;
use crate::models::projects::{
    LiveReleaseByRevisionApiResponse, PlatformEnvironmentType, ProjectDetailResponse,
    ProjectInsert, ProjectReleaseResponse, ProjectReleaseWithBaseline, ProjectResponse,
    ProjectRuntimeInsert, ProjectRuntimeResponse, ProjectRuntimeWithLiveReleaseApi,
    ProjectRuntimeWithLiveReleaseResponse, ProjectRuntimesBaseUrls, ProjectUnitResponse,
    ProjectUnitType,
};
use crate::routes::chat::handlers::{
    broadcast_production_live_release_changed, broadcast_stage_release_create_effects,
    http_response_for_stage_release_from_controller_error,
};
use crate::routes::ws::events::{ProjectDeletedPayload, WsServerEvent};
use crate::routes::ws::{publish_event_to_channel, user_channel};
use crate::schema::chat_messages;
use crate::schema::chat_sessions;
use crate::schema::project_releases;
use crate::schema::project_runtimes;
use crate::schema::project_units;
use crate::schema::projects;
use crate::services::chat_worker::db::{
    list_distinct_context_slugs_from_units, load_project_units_by_keys,
    load_units_from_latest_production_metadata, modules_by_context_slugs_from_units,
};
use crate::services::payment::{
    PaidFeature, most_complete_needed_package, user_missing_paid_features,
};
use crate::services::observability::{
    fetch_jaeger_trace_by_id, JaegerQueryError, RELEASE_ID_TAG_KEY, RUNTIME_ID_TAG_KEY,
    trace_baal_project_id_matches, trace_first_tag_string, trace_optional_unique_tag_uuid,
    UNIT_ID_TAG_KEY, UNIT_PATH_TAG_KEY,
};
use crate::services::releases::{
    ProjectReleaseRevisionLookup, attach_base_production_releases,
    attach_is_draft_flags_from_latest_production, combined_units_from_release_metadata,
    load_unit_previous_revisions_for_release, pick_unit_from_combined_snapshot,
    project_release_id_for_revision_lane,
    recompute_user_message_edit_disabled_for_session_live_production,
};
use crate::services::runtime_secrets::insert_runtime_encryption_keys_for_runtimes;
use crate::services::stage_release_from_controller::transaction_create_stage_release_from_controller;
use crate::services::stage_runtime_release::{
    RuntimeDeployDenied, RuntimeDetachDenied, StageRuntimeDeployDenied,
    apply_release_deploy_to_runtime, apply_runtime_detach_live_release,
    apply_stage_release_deploy_to_runtime,
};
use crate::{libs::api_response::api_success, routes::AppContext};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::prelude::*;
use diesel::result::Error;
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
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

/// Body for [`handle_detach_runtime_live_release`]: matches frontend union (revision | controller | detach-only).
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

fn resolve_runtime_environment(
    conn: &mut database::PooledConn,
    user_id: Uuid,
    project_id: Uuid,
    runtime_id: Uuid,
) -> Result<Option<PlatformEnvironmentType>, diesel::result::Error> {
    project_runtimes::table
        .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .filter(project_runtimes::project_id.eq(project_id))
        .filter(project_runtimes::id.eq(runtime_id))
        .select(project_runtimes::environment)
        .first::<PlatformEnvironmentType>(conn)
        .optional()
}

fn env_display_label(env: &PlatformEnvironmentType) -> &'static str {
    match env {
        PlatformEnvironmentType::Stage => "Stage",
        PlatformEnvironmentType::Prod => "Production",
    }
}

/// Dispatches NATS notification based on the runtime's environment.
async fn notify_runtime_release(
    context: &web::Data<AppContext>,
    env: &PlatformEnvironmentType,
    project_id: Uuid,
    runtime_id: Uuid,
    deployed_release_id: Option<Uuid>,
    previous_release_id: Option<Uuid>,
    op_label: &'static str,
) {
    match env {
        PlatformEnvironmentType::Stage => {
            notify_stage_runtime_release_nats(
                context,
                project_id,
                runtime_id,
                deployed_release_id,
                previous_release_id,
                op_label,
            )
            .await;
        }
        PlatformEnvironmentType::Prod => {
            if let Some(deployed_id) = deployed_release_id {
                if let Some(ref nats) = context.runtime_nats_publisher {
                    let subject = context.settings.runtime_nats_subject_release.as_str();
                    if let Err(e) = nats
                        .publish_prod_release(subject, project_id, deployed_id, previous_release_id)
                        .await
                    {
                        tracing::warn!(
                            op = op_label,
                            "[{}] NATS prod publish failed: {}",
                            op_label,
                            e
                        );
                    }
                }
            }
            broadcast_production_live_release_changed(context, project_id).await;
        }
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

            let runtimes: Vec<ProjectRuntimeResponse> = diesel::insert_into(project_runtimes::table)
                .values(&[
                    ProjectRuntimeInsert {
                        project_id: project.id,
                        environment: PlatformEnvironmentType::Stage,
                        name: Some("Stage 1".to_string()),
                        slug: Some("stage-1".to_string()),
                    },
                    ProjectRuntimeInsert {
                        project_id: project.id,
                        environment: PlatformEnvironmentType::Stage,
                        name: Some("Stage 2".to_string()),
                        slug: Some("stage-2".to_string()),
                    },
                    ProjectRuntimeInsert {
                        project_id: project.id,
                        environment: PlatformEnvironmentType::Prod,
                        name: Some("Production".to_string()),
                        slug: Some("prod".to_string()),
                    },
                ])
                .returning(ProjectRuntimeResponse::as_returning())
                .get_results(conn)?;

            let runtime_ids: Vec<Uuid> = runtimes.iter().map(|r| r.id).collect();
            insert_runtime_encryption_keys_for_runtimes(conn, &runtime_ids)
                .map_err(|e| Error::QueryBuilderError(Box::new(e)))?;

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

    let payload = ProjectDetailResponse {
        project,
        runtimes: ProjectRuntimesBaseUrls::uniform(&context.settings.base_runtime_endpoint),
    };

    HttpResponse::Ok().json(api_success("Project fetched successfully", json!(payload)))
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

#[derive(Deserialize)]
pub struct ListProjectReleasesQuery {
    pub environment: Option<PlatformEnvironmentType>,
}

/// Query for [`handle_get_project_release_by_revision`]: `environment` is `stage` or `prod`; `revision`
/// is the lane revision. 404 if no row exists, 400 if none of those rows are live on a runtime in
/// that environment, 200 with one entry per matching runtime (same numeric revision can map to
/// different `project_releases.id` values on different stage runtimes).
#[derive(Deserialize)]
pub struct GetProjectReleaseByRevisionQuery {
    pub environment: PlatformEnvironmentType,
    pub revision: f64,
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
                project_releases::table
                    .on(project_runtimes::live_release_id.eq(project_releases::id.nullable())),
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
        let by_id: HashMap<Uuid, ProjectReleaseWithBaseline> = if to_enrich.is_empty() {
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

/// Detach live release and optionally rebind: by existing revision, by release-controller
/// message (stage only), or `{}` for detach-only. Works for both stage and production runtimes.
pub async fn handle_detach_runtime_live_release(
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
            handle_detach_by_revision(
                &context,
                user_id,
                project_id,
                runtime_id,
                revision,
                chat_session_id,
            )
            .await
        }
        DetachLiveReleaseBody::ByController {
            chat_session_id,
            release_controller_message_id,
        } => {
            handle_detach_by_controller(
                &context,
                user_id,
                project_id,
                runtime_id,
                chat_session_id,
                release_controller_message_id,
            )
            .await
        }
        DetachLiveReleaseBody::DetachOnly(_) => {
            handle_detach_only(&context, user_id, project_id, runtime_id).await
        }
    }
}

async fn handle_detach_by_revision(
    context: &web::Data<AppContext>,
    user_id: Uuid,
    project_id: Uuid,
    runtime_id: Uuid,
    revision: f64,
    chat_session_id: Uuid,
) -> HttpResponse {
    let outcome = match database::block_on_db(&context.pool, move |conn| {
        let Some(env) = resolve_runtime_environment(conn, user_id, project_id, runtime_id)? else {
            return Ok(None);
        };
        let result = apply_release_deploy_to_runtime(
            conn,
            user_id,
            project_id,
            runtime_id,
            revision,
            chat_session_id,
            env.clone(),
        )?;
        Ok(Some((env, result)))
    })
    .await
    {
        Ok(o) => o,
        Err(_) => {
            log::error!("[handle_detach_runtime_live_release] DB/block error (by revision)");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let Some((env, deploy_result)) = outcome else {
        return HttpResponse::NotFound().json(api_error("Project runtime not found", json!({})));
    };

    let env_label = env_display_label(&env);
    match deploy_result {
        Ok((rt, deployed_release_id, previous_release_id)) => {
            notify_runtime_release(
                context,
                &env,
                project_id,
                runtime_id,
                Some(deployed_release_id),
                previous_release_id,
                "handle_detach_runtime_live_release",
            )
            .await;
            HttpResponse::Ok().json(api_success(
                &format!("{} release deployed on runtime", env_label),
                json!({ "runtime": rt }),
            ))
        }
        Err(RuntimeDeployDenied::NotFound) => {
            HttpResponse::NotFound().json(api_error("Project runtime not found", json!({})))
        }
        Err(RuntimeDeployDenied::EnvironmentMismatch) => {
            HttpResponse::BadRequest().json(api_error("Runtime environment mismatch", json!({})))
        }
        Err(RuntimeDeployDenied::NoSuchRelease) => HttpResponse::BadRequest().json(api_error(
            &format!(
                "No {} release with this revision exists for this project and chat session",
                env_label.to_lowercase()
            ),
            json!({}),
        )),
        Err(RuntimeDeployDenied::AmbiguousRevision) => HttpResponse::BadRequest().json(api_error(
            &format!(
                "Multiple {} release rows match this project, chat session, and revision",
                env_label.to_lowercase()
            ),
            json!({}),
        )),
    }
}

/// Release-controller flow is stage-only by design.
async fn handle_detach_by_controller(
    context: &web::Data<AppContext>,
    user_id: Uuid,
    project_id: Uuid,
    runtime_id: Uuid,
    chat_session_id: Uuid,
    release_controller_message_id: Uuid,
) -> HttpResponse {
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
            log::error!("[handle_detach_runtime_live_release] DB/block error (by controller)");
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
                        "[handle_detach_runtime_live_release] DB/block error (attach baseline)"
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

async fn handle_detach_only(
    context: &web::Data<AppContext>,
    user_id: Uuid,
    project_id: Uuid,
    runtime_id: Uuid,
) -> HttpResponse {
    let outcome = match database::block_on_db(&context.pool, move |conn| {
        apply_runtime_detach_live_release(conn, user_id, project_id, runtime_id)
    })
    .await
    {
        Ok(o) => o,
        Err(_) => {
            log::error!("[handle_detach_runtime_live_release] DB/block error (detach only)");
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
                env = ?rt.environment,
                "detach-only succeeded in DB; publishing NATS event (clear live release for runtime)"
            );
            notify_runtime_release(
                context,
                &rt.environment,
                project_id,
                runtime_id,
                None,
                previous_release_id,
                "handle_detach_runtime_live_release",
            )
            .await;
            HttpResponse::Ok().json(api_success("Runtime detached from live release", json!(rt)))
        }
        Err(RuntimeDetachDenied::NotFound) => {
            HttpResponse::NotFound().json(api_error("Project runtime not found", json!({})))
        }
        Err(RuntimeDetachDenied::NotConnected) => HttpResponse::BadRequest().json(api_error(
            "Runtime is not connected to a live release",
            json!({}),
        )),
    }
}

pub async fn handle_list_project_releases(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    query: web::Query<ListProjectReleasesQuery>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list project releases");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;
    let env_filter = query.environment.clone();

    let releases = match database::block_on_db(&context.pool, move |conn| {
        let mut q = project_releases::table
            .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_releases::project_id.eq(project_id))
            .into_boxed();
        if let Some(env) = env_filter {
            let is_stage = env == PlatformEnvironmentType::Stage;
            q = q.filter(project_releases::is_stage.eq(is_stage));
        }
        let rows: Vec<ProjectReleaseResponse> = q
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

pub async fn handle_get_project_release_by_revision(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    query: web::Query<GetProjectReleaseByRevisionQuery>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get project release by revision");

    let project_id = path.into_inner();
    let user_id = access_payload.user_id;
    let GetProjectReleaseByRevisionQuery {
        environment: rt_env,
        revision,
    } = query.into_inner();
    let is_stage = rt_env == PlatformEnvironmentType::Stage;

    enum LookupOutcome {
        NotFound,
        NotLive,
        Ok(LiveReleaseByRevisionApiResponse),
    }

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        let release_ids: Vec<Uuid> = project_releases::table
            .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_releases::project_id.eq(project_id))
            .filter(project_releases::is_stage.eq(is_stage))
            .filter(project_releases::revision.eq(revision))
            .select(project_releases::id)
            .load::<Uuid>(conn)?;

        if release_ids.is_empty() {
            return Ok(LookupOutcome::NotFound);
        }

        let rows: Vec<ProjectRuntimeWithLiveReleaseResponse> = project_runtimes::table
            .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
            .left_join(
                project_releases::table
                    .on(project_runtimes::live_release_id.eq(project_releases::id.nullable())),
            )
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_runtimes::project_id.eq(project_id))
            .filter(project_runtimes::environment.eq(rt_env))
            .filter(project_runtimes::live_release_id.eq_any(&release_ids))
            .order(project_runtimes::id.asc())
            .select(ProjectRuntimeWithLiveReleaseResponse::as_select())
            .load::<ProjectRuntimeWithLiveReleaseResponse>(conn)?;

        if rows.is_empty() {
            return Ok(LookupOutcome::NotLive);
        }

        let to_enrich: Vec<ProjectReleaseResponse> =
            rows.iter().filter_map(|r| r.live_release.clone()).collect();
        let by_id: HashMap<Uuid, ProjectReleaseWithBaseline> = if to_enrich.is_empty() {
            HashMap::new()
        } else {
            attach_base_production_releases(conn, project_id, to_enrich)?
                .into_iter()
                .map(|e| (e.release.id, e))
                .collect()
        };
        let runtimes: Vec<ProjectRuntimeWithLiveReleaseApi> = rows
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
            .collect();

        Ok(LookupOutcome::Ok(LiveReleaseByRevisionApiResponse { runtimes }))
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[handle_get_project_release_by_revision] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    match outcome {
        LookupOutcome::NotFound => HttpResponse::NotFound().json(api_error(
            "No release with this revision exists for this environment",
            json!({}),
        )),
        LookupOutcome::NotLive => HttpResponse::BadRequest().json(api_error(
            "A release with this revision exists but it is not the live release for any runtime in this environment",
            json!({}),
        )),
        LookupOutcome::Ok(enriched) => HttpResponse::Ok().json(api_success(
            "Live release fetched successfully",
            json!(enriched),
        )),
    }
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

const MAX_RELEASE_UNITS_PATH_ID_FILTER_ITEMS: usize = 1_000;

fn release_unit_path_key(u: &ProjectUnitResponse) -> String {
    format!(
        "{}.{}.{}.{}",
        u.context_slug,
        u.module_slug,
        u.unit_type.as_slug(),
        u.unit_name_slug
    )
}

fn normalize_release_unit_path_filters(paths: &[String]) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for p in paths {
        let t = p.trim();
        if t.is_empty() {
            continue;
        }
        let n = normalize_full_unit_id_arg(t);
        let parsed = parse_unit_path(&n)?;
        out.insert(format!(
            "{}.{}.{}.{}",
            parsed.context_slug,
            parsed.module_slug,
            parsed.unit_type.as_slug(),
            parsed.unit_name_slug
        ));
    }
    Ok(out)
}

fn normalize_release_unit_id_filters(ids: &[String]) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for id in ids {
        let n = normalize_full_unit_id_arg(id);
        if n.is_empty() {
            continue;
        }
        validate_full_unit_id_for_tool(&n)?;
        out.insert(n);
    }
    Ok(out)
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
    #[serde(default)]
    pub units_paths: Vec<String>,
    #[serde(default)]
    pub units_ids: Vec<String>,
}

/// Filters, sorts, and optionally paginates in-memory.
fn filter_sort_paginate_release_units(
    mut units: Vec<ProjectUnitResponse>,
    unit_types: &[ProjectUnitType],
    context_slugs: &[String],
    context_module_pairs: &[ContextModulePair],
    path_filter: Option<&HashSet<String>>,
    id_filter: Option<&HashSet<String>>,
    skip_pagination: bool,
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
            .map(|p| {
                (
                    p.context_slug.trim().to_string(),
                    p.module_slug.trim().to_string(),
                )
            })
            .filter(|(c, m)| !c.is_empty() && !m.is_empty())
            .collect();
        if v.is_empty() {
            None
        } else {
            Some(v.into_iter().collect())
        }
    };

    units.retain(|u| {
        if let Some(ref ts) = type_set
            && !ts.contains(&u.unit_type)
        {
            return false;
        }
        if let Some(ref cs) = ctx_set
            && !cs.contains(u.context_slug.as_str())
        {
            return false;
        }
        if let Some(ref ps) = pair_set
            && !ps.contains(&(u.context_slug.clone(), u.module_slug.clone()))
        {
            return false;
        }
        if path_filter.is_some() || id_filter.is_some() {
            let path_ok = path_filter.is_some_and(|s| s.contains(&release_unit_path_key(u)));
            let nid = normalize_full_unit_id_arg(&u.id);
            let id_ok = id_filter.is_some_and(|s| s.contains(&nid));
            if !path_ok && !id_ok {
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
    if skip_pagination {
        let n = units.len() as u32;
        return (units, total, 1, n);
    }
    let page = page.max(1);
    let page_size = page_size.clamp(1, 100);
    let offset = (page.saturating_sub(1) as usize).saturating_mul(page_size as usize);
    let page_units: Vec<_> = units
        .into_iter()
        .skip(offset)
        .take(page_size as usize)
        .collect();
    (page_units, total, page, page_size)
}

/// Combined release snapshot (`combinedUnitIDs`). Filters live in the JSON body; if `units_paths` and/or
/// `units_ids` resolve to at least one value, every matching row is returned (no pagination).
/// [`ListReleaseUnitsAllBody::context_module_pairs`] sends `context_slug` and `module_slug` per row.
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

    if body.units_paths.len().saturating_add(body.units_ids.len()) > MAX_RELEASE_UNITS_PATH_ID_FILTER_ITEMS
    {
        return HttpResponse::BadRequest().json(api_error(
            "Too many units_paths or units_ids entries in one request",
            json!({ "max_total": MAX_RELEASE_UNITS_PATH_ID_FILTER_ITEMS }),
        ));
    }

    let path_keys = match normalize_release_unit_path_filters(&body.units_paths) {
        Ok(v) => v,
        Err(msg) => {
            return HttpResponse::BadRequest()
                .json(api_error("Invalid units_paths entry", json!({ "detail": msg })));
        }
    };
    let id_keys = match normalize_release_unit_id_filters(&body.units_ids) {
        Ok(v) => v,
        Err(msg) => {
            return HttpResponse::BadRequest()
                .json(api_error("Invalid units_ids entry", json!({ "detail": msg })));
        }
    };

    let path_filter: Option<HashSet<String>> = (!path_keys.is_empty()).then_some(path_keys);
    let id_filter: Option<HashSet<String>> = (!id_keys.is_empty()).then_some(id_keys);
    let skip_pagination = path_filter.is_some() || id_filter.is_some();

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
            let page_size_out = if skip_pagination {
                0
            } else {
                page_size_req
            };
            return Ok(Some((empty, 0u64, page_req, page_size_out)));
        }

        let units = load_project_units_by_keys(conn, project_id, &keys)?;
        let units = attach_is_draft_flags_from_latest_production(conn, project_id, units)?;
        let (page_units, total, page, page_size) = filter_sort_paginate_release_units(
            units,
            &unit_types,
            &context_slugs,
            &context_module_pairs,
            path_filter.as_ref(),
            id_filter.as_ref(),
            skip_pagination,
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

const MAX_RELEASE_UNIT_PREVIOUS_BY_IDS: usize = 100;

#[derive(Debug, Deserialize)]
pub struct ReleaseUnitPreviousByFullIdsBody {
    pub unit_ids: Vec<String>,
}

#[derive(Serialize)]
struct ReleaseUnitPreviousByFullIdsResponse {
    units: Vec<Option<ProjectUnitResponse>>,
}

enum ReleaseUnitPreviousLookupErr {
    ReleaseNotFound,
    UnitNotInReleaseNewSet(String),
}

pub async fn handle_release_unit_previous_by_full_ids(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<ReleaseUnitPreviousByFullIdsBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to resolve release unit previous revisions");

    let (project_id, release_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let unit_ids = &body.unit_ids;

    if unit_ids.len() > MAX_RELEASE_UNIT_PREVIOUS_BY_IDS {
        return HttpResponse::BadRequest().json(api_error(
            "Too many unit ids in one request",
            json!({ "max": MAX_RELEASE_UNIT_PREVIOUS_BY_IDS }),
        ));
    }

    for id in unit_ids {
        if let Err(msg) = validate_full_unit_id_for_tool(id) {
            return HttpResponse::BadRequest().json(api_error(
                "Invalid full unit id",
                json!({ "unit_id": id, "detail": msg }),
            ));
        }
    }

    let response = match database::block_on_db(&context.pool, {
        let unit_ids = unit_ids.clone();
        move |conn| {
            let release: Option<ProjectReleaseResponse> = project_releases::table
                .inner_join(projects::table.on(project_releases::project_id.eq(projects::id)))
                .filter(projects::owner_user_id.eq(user_id))
                .filter(project_releases::project_id.eq(project_id))
                .filter(project_releases::id.eq(release_id))
                .select(ProjectReleaseResponse::as_select())
                .first(conn)
                .optional()?;

            let Some(release) = release else {
                return Ok(Err(ReleaseUnitPreviousLookupErr::ReleaseNotFound));
            };

            let (units, missing) =
                load_unit_previous_revisions_for_release(conn, project_id, &release, &unit_ids)?;
            if let Some(id) = missing {
                return Ok(Err(ReleaseUnitPreviousLookupErr::UnitNotInReleaseNewSet(
                    id,
                )));
            }
            Ok(Ok(ReleaseUnitPreviousByFullIdsResponse { units }))
        }
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(ReleaseUnitPreviousLookupErr::ReleaseNotFound)) => {
            return HttpResponse::NotFound()
                .json(api_error("Project release not found", json!({})));
        }
        Ok(Err(ReleaseUnitPreviousLookupErr::UnitNotInReleaseNewSet(unit_id))) => {
            return HttpResponse::NotFound().json(api_error(
                "Unit id is not exactly one row in this release new units metadata",
                json!({ "unit_id": unit_id }),
            ));
        }
        Err(database::BlockingDbError::Query(e)) => {
            log::error!(
                "[handle_release_unit_previous_by_full_ids] DB error: {:?}",
                e
            );
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            log::error!("[handle_release_unit_previous_by_full_ids] pool/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Release previous unit revisions resolved successfully",
        json!(response),
    ))
}

/// Distinct bounded-context slugs (`context_slug`) from this release’s full snapshot (`combinedUnitIDs`),
/// same scope as [`handle_list_project_release_units_all`].
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
                m.combined_unit_refs
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

/// For each requested bounded-context slug, distinct `module_slug` values from this release’s full
/// snapshot (`combinedUnitIDs`), same scope as [`handle_list_project_release_contexts`].
pub async fn handle_list_project_release_modules_by_contexts(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<ModulesByContextsBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();

    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list modules by contexts");

    let (project_id, release_id) = path.into_inner();
    let user_id = access_payload.user_id;

    let has_slugs = body.context_slugs.iter().any(|s| !s.trim().is_empty());
    if !has_slugs {
        return HttpResponse::BadRequest().json(api_error(
            "context_slugs must contain at least one non-empty context slug",
            json!({}),
        ));
    }

    let slugs = body.context_slugs.clone();

    let by_context = match database::block_on_db(&context.pool, move |conn| {
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

        let units = if keys.is_empty() {
            vec![]
        } else {
            load_project_units_by_keys(conn, project_id, &keys)?
        };

        let rows = modules_by_context_slugs_from_units(&units, &slugs);
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
                "[handle_list_project_release_modules_by_contexts] Failed to load: {:?}",
                e
            );
            return HttpResponse::InternalServerError().json(api_error(&e.to_string(), json!({})));
        }
        Err(_) => {
            log::error!("[handle_list_project_release_modules_by_contexts] DB/block error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let Some(by_context) = by_context else {
        return HttpResponse::NotFound().json(api_error("Project release not found", json!({})));
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
        Err(StageRuntimeDeployDenied::AmbiguousRevision) => {
            HttpResponse::BadRequest().json(api_error(
                "Multiple stage release rows match this project, chat session, and revision",
                json!({}),
            ))
        }
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

        recompute_user_message_edit_disabled_for_session_live_production(
            conn,
            project_id,
            chat_session_id,
        )?;

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
                    tracing::warn!(
                        "[handle_deploy_production_release] NATS publish failed: {}",
                        e
                    );
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

const MAX_PROJECT_TRACE_IDS: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadProjectTracesByIdsBody {
    pub trace_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadedProjectTraceEntry {
    trace_id: String,
    trace: Value,
    release: Option<ProjectReleaseResponse>,
    runtime: Option<ProjectRuntimeResponse>,
    unit: Option<ProjectUnitResponse>,
}

fn validate_jaeger_trace_id_param(trace_id: &str) -> Result<(), &'static str> {
    if !trace_id
        .chars()
        .all(|c| c.is_ascii_hexdigit())
    {
        return Err("each traceId must contain only hexadecimal characters");
    }
    if trace_id.len() != 16 && trace_id.len() != 32 {
        return Err("each traceId must be exactly 16 or 32 hex characters");
    }
    Ok(())
}

fn jaeger_query_err_http(err: JaegerQueryError) -> HttpResponse {
    match err {
        JaegerQueryError::ClientBuild(_) => HttpResponse::InternalServerError()
            .json(api_error("Failed to build HTTP client", json!({}))),
        JaegerQueryError::Transport { .. } => {
            HttpResponse::BadGateway().json(api_error("Failed to reach Jaeger", json!({})))
        }
        JaegerQueryError::BadStatus { status, .. } => HttpResponse::BadGateway().json(api_error(
            &format!("Jaeger returned status {}", status),
            json!({}),
        )),
        JaegerQueryError::InvalidPayload { .. } => {
            HttpResponse::BadGateway().json(api_error("Invalid response from Jaeger", json!({})))
        }
    }
}

struct TracePlan {
    trace_id: String,
    trace: Value,
    release_id: Option<Uuid>,
    runtime_id: Option<Uuid>,
    unit_path: Option<String>,
    unit_id: Option<String>,
}

/// Loads full Jaeger trace JSON for the given ids, verifies `baal.project.id` on each trace matches
/// the path project, optionally attaches the release row, runtime row, and a unit row resolved only
/// from that release’s combined snapshot metadata (no path/id DB fallback outside the snapshot).
pub async fn handle_load_project_traces_by_ids(
    context: web::Data<AppContext>,
    path: web::Path<Uuid>,
    body: web::Json<LoadProjectTracesByIdsBody>,
) -> HttpResponse {
    let project_id = path.into_inner();
    let normalized: Vec<String> = body
        .trace_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if normalized.is_empty() {
        return HttpResponse::BadRequest().json(api_error(
            "traceIds must contain at least one non-empty id",
            json!({}),
        ));
    }
    if normalized.len() > MAX_PROJECT_TRACE_IDS {
        return HttpResponse::BadRequest().json(api_error(
            &format!("traceIds must contain at most {} ids", MAX_PROJECT_TRACE_IDS),
            json!({}),
        ));
    }

    for tid in &normalized {
        if let Err(msg) = validate_jaeger_trace_id_param(tid) {
            return HttpResponse::BadRequest().json(api_error(msg, json!({})));
        }
    }

    let mut uniq: Vec<String> = Vec::new();
    for t in &normalized {
        if !uniq.iter().any(|u| u == t) {
            uniq.push(t.clone());
        }
    }

    let jaeger_base = context.settings.jaeger_query_base_url.clone();
    let fetched = match try_join_all(uniq.iter().map(|tid| {
        let base = jaeger_base.clone();
        let tid = tid.clone();
        async move { fetch_jaeger_trace_by_id(&base, &tid).await }
    }))
    .await
    {
        Ok(v) => v,
        Err(e) => return jaeger_query_err_http(e),
    };

    if fetched.iter().any(|o| o.is_none()) {
        return HttpResponse::NotFound().json(api_error(
            "One or more traces were not found in Jaeger",
            json!({}),
        ));
    }

    let by_id: HashMap<String, Value> = uniq
        .into_iter()
        .zip(fetched.into_iter().map(|o| o.expect("checked non-none")))
        .collect();

    let mut plans: Vec<TracePlan> = Vec::with_capacity(normalized.len());
    for tid in &normalized {
        let Some(trace) = by_id.get(tid).cloned() else {
            log::error!(
                "[handle_load_project_traces_by_ids] missing trace after fetch trace_id={}",
                tid
            );
            return HttpResponse::InternalServerError()
                .json(api_error("Internal error resolving trace payload", json!({})));
        };
        if let Err(msg) = trace_baal_project_id_matches(&trace, project_id) {
            return HttpResponse::Forbidden().json(api_error(msg, json!({})));
        }
        let release_id = match trace_optional_unique_tag_uuid(&trace, RELEASE_ID_TAG_KEY) {
            Ok(v) => v,
            Err(msg) => return HttpResponse::BadRequest().json(api_error(msg, json!({}))),
        };
        let runtime_id = match trace_optional_unique_tag_uuid(&trace, RUNTIME_ID_TAG_KEY) {
            Ok(v) => v,
            Err(msg) => return HttpResponse::BadRequest().json(api_error(msg, json!({}))),
        };
        let unit_path = trace_first_tag_string(&trace, UNIT_PATH_TAG_KEY);
        let unit_id = trace_first_tag_string(&trace, UNIT_ID_TAG_KEY);
        plans.push(TracePlan {
            trace_id: tid.clone(),
            trace,
            release_id,
            runtime_id,
            unit_path,
            unit_id,
        });
    }

    let mut release_ids: HashSet<Uuid> = HashSet::new();
    let mut runtime_ids: HashSet<Uuid> = HashSet::new();
    let mut releases_needing_combined: HashSet<Uuid> = HashSet::new();
    for p in &plans {
        if let Some(r) = p.release_id {
            release_ids.insert(r);
            if p.unit_path.is_some() || p.unit_id.is_some() {
                releases_needing_combined.insert(r);
            }
        }
        if let Some(r) = p.runtime_id {
            runtime_ids.insert(r);
        }
    }

    let release_ids_vec: Vec<Uuid> = release_ids.iter().copied().collect();
    let runtime_ids_vec: Vec<Uuid> = runtime_ids.iter().copied().collect();
    let releases_for_combined: Vec<Uuid> = releases_needing_combined.iter().copied().collect();

    let (releases, runtimes, combined_by_release) = match database::block_on_db(
        &context.pool,
        move |conn| {
            let mut releases: HashMap<Uuid, ProjectReleaseResponse> = HashMap::new();
            if !release_ids_vec.is_empty() {
                let rows: Vec<ProjectReleaseResponse> = project_releases::table
                    .filter(project_releases::project_id.eq(project_id))
                    .filter(project_releases::id.eq_any(&release_ids_vec))
                    .select(ProjectReleaseResponse::as_select())
                    .load(conn)?;
                for r in rows {
                    releases.insert(r.id, r);
                }
            }

            let mut runtimes: HashMap<Uuid, ProjectRuntimeResponse> = HashMap::new();
            if !runtime_ids_vec.is_empty() {
                let rows: Vec<ProjectRuntimeResponse> = project_runtimes::table
                    .filter(project_runtimes::project_id.eq(project_id))
                    .filter(project_runtimes::id.eq_any(&runtime_ids_vec))
                    .select(ProjectRuntimeResponse::as_select())
                    .load(conn)?;
                for r in rows {
                    runtimes.insert(r.id, r);
                }
            }

            let mut combined_by_release: HashMap<Uuid, Vec<ProjectUnitResponse>> = HashMap::new();
            for rid in &releases_for_combined {
                let Some(rel) = releases.get(rid) else {
                    continue;
                };
                let rows =
                    combined_units_from_release_metadata(conn, project_id, rel.units_metadata.as_ref())?;
                combined_by_release.insert(*rid, rows);
            }

            Ok((releases, runtimes, combined_by_release))
        },
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            log::error!("[handle_load_project_traces_by_ids] DB/block error: {:?}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    for rid in &release_ids {
        if !releases.contains_key(rid) {
            return HttpResponse::NotFound().json(api_error(
                "One or more releases referenced by traces were not found for this project",
                json!({}),
            ));
        }
    }
    for rid in &runtime_ids {
        if !runtimes.contains_key(rid) {
            return HttpResponse::NotFound().json(api_error(
                "One or more runtimes referenced by traces were not found for this project",
                json!({}),
            ));
        }
    }

    let mut entries: Vec<LoadedProjectTraceEntry> = Vec::with_capacity(plans.len());
    for p in plans {
        let release = p.release_id.and_then(|id| releases.get(&id).cloned());
        let runtime = p.runtime_id.and_then(|id| runtimes.get(&id).cloned());
        let unit = if p.unit_path.is_some() || p.unit_id.is_some() {
            match p.release_id {
                Some(rid) => combined_by_release.get(&rid).and_then(|comb| {
                    pick_unit_from_combined_snapshot(
                        comb,
                        p.unit_id.as_deref(),
                        p.unit_path.as_deref(),
                    )
                }),
                None => None,
            }
        } else {
            None
        };

        let entry = LoadedProjectTraceEntry {
            trace_id: p.trace_id,
            trace: p.trace,
            release,
            runtime,
            unit,
        };
        entries.push(entry);
    }

    HttpResponse::Ok().json(api_success(
        "Traces loaded successfully",
        json!({ "traces": entries }),
    ))
}
