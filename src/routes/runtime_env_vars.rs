use crate::database::{self, BlockingDbError};
use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::AccessPayload;
use crate::models::runtime_env_vars::{
    ProjectRuntimeEnvVarInsert, ProjectRuntimeEnvVarRow, RuntimeEnvVarGroupedResponse,
    RuntimeEnvVarRevealResponse,
};
use crate::routes::AppContext;
use crate::schema::project_runtime_env_vars;
use crate::schema::project_runtimes;
use crate::schema::projects;
use crate::services::runtime_secrets::{
    self, RuntimeSecretsError, ensure_runtime_encryption_key, prepare_stored_value,
    runtime_key_from_row,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use diesel::dsl::now;
use diesel::prelude::*;
use diesel::upsert::excluded;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

const MAX_ENV_KEY_LEN: usize = 255;
const MAX_ENV_VALUE_LEN: usize = 64 * 1024;

fn dedupe_invalidations(invalidations: Vec<(Uuid, String)>) -> Vec<(Uuid, String)> {
    let mut seen = HashSet::new();
    invalidations
        .into_iter()
        .filter(|pair| seen.insert(pair.clone()))
        .collect()
}

async fn notify_runtime_env_cache_invalidate(
    context: &web::Data<AppContext>,
    project_id: Uuid,
    invalidations: &[(Uuid, String)],
    op_label: &'static str,
) {
    let invalidations = dedupe_invalidations(invalidations.to_vec());
    if invalidations.is_empty() {
        return;
    }
    let subject = context.settings.runtime_nats_subject_env.as_str();
    let Some(ref nats) = context.runtime_nats_publisher else {
        tracing::warn!(
            target: "runtime_nats",
            op = op_label,
            subject = %subject,
            project_id = %project_id,
            count = invalidations.len(),
            "runtime NATS publisher not configured; runtimes will not invalidate env cache via NATS"
        );
        return;
    };
    tracing::info!(
        target: "runtime_nats",
        op = op_label,
        subject = %subject,
        project_id = %project_id,
        count = invalidations.len(),
        "notifying runtimes via NATS (env cache invalidation)"
    );
    for (runtime_id, key) in &invalidations {
        if let Err(e) = nats
            .publish_runtime_env_invalidate(subject, project_id, *runtime_id, key.as_str())
            .await
        {
            tracing::warn!(
                target: "runtime_nats",
                op = op_label,
                subject = %subject,
                project_id = %project_id,
                runtime_id = %runtime_id,
                key = %key.as_str(),
                error = %e,
                "NATS env invalidation publish failed after DB success; runtime cache may be stale"
            );
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct UpsertRuntimeEnvVarItem {
    pub key: String,
    #[serde(default, rename = "isEncrypted")]
    pub is_encrypted: Option<bool>,
    #[serde(flatten)]
    pub runtime_values: HashMap<String, Value>,
}

#[derive(Clone, Deserialize)]
pub struct UpsertRuntimeEnvVarsBatchBody {
    pub items: Vec<UpsertRuntimeEnvVarItem>,
}

#[derive(Deserialize)]
pub struct DeleteRuntimeEnvVarsBatchBody {
    pub keys: Vec<String>,
}

#[derive(Deserialize)]
pub struct ListRuntimeEnvVarsQuery {
    #[serde(rename = "isEncrypted")]
    pub is_encrypted: Option<bool>,
}

fn parse_runtime_values(
    raw: HashMap<String, Value>,
) -> Result<HashMap<Uuid, String>, &'static str> {
    let mut out = HashMap::new();
    for (field, value) in raw {
        let runtime_id = Uuid::parse_str(&field).map_err(|_| "invalid_runtime_id")?;
        let s = match value {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.as_str().map(|s| s.to_string()).ok_or("invalid_runtime_value")?,
        };
        if s.len() > MAX_ENV_VALUE_LEN {
            return Err("value_too_long");
        }
        out.insert(runtime_id, s);
    }
    Ok(out)
}

fn validate_env_key(key: &str) -> Result<(), &'static str> {
    if key.is_empty() || key.len() > MAX_ENV_KEY_LEN {
        return Err("invalid_key");
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return Err("invalid_key");
    }
    Ok(())
}

fn normalize_env_key(raw: &str) -> Result<String, &'static str> {
    let key = raw.trim().to_ascii_uppercase();
    validate_env_key(&key)?;
    Ok(key)
}

fn load_project_runtime_ids(
    conn: &mut diesel::PgConnection,
    project_id: Uuid,
    user_id: Uuid,
) -> QueryResult<HashSet<Uuid>> {
    project_runtimes::table
        .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .filter(projects::id.eq(project_id))
        .select(project_runtimes::id)
        .load::<Uuid>(conn)
        .map(|ids| ids.into_iter().collect())
}

fn validate_runtime_ids_subset(
    allowed: &HashSet<Uuid>,
    provided: &HashMap<Uuid, String>,
) -> Result<(), &'static str> {
    if provided.is_empty() {
        return Err("no_runtime_values");
    }
    for runtime_id in provided.keys() {
        if !allowed.contains(runtime_id) {
            return Err("invalid_runtime_id");
        }
    }
    Ok(())
}

struct ParsedEnvVarItem {
    key: String,
    is_encrypted: bool,
    runtime_values: HashMap<Uuid, String>,
}

fn parse_upsert_items(
    items: Vec<UpsertRuntimeEnvVarItem>,
    allowed_runtime_ids: &HashSet<Uuid>,
) -> Result<Vec<ParsedEnvVarItem>, &'static str> {
    if items.is_empty() {
        return Err("empty_batch");
    }

    let mut seen_keys = HashSet::new();
    let mut parsed = Vec::with_capacity(items.len());

    for item in items {
        let key = normalize_env_key(&item.key)?;
        if !seen_keys.insert(key.clone()) {
            return Err("duplicate_key");
        }

        let runtime_values = parse_runtime_values(item.runtime_values)?;
        validate_runtime_ids_subset(allowed_runtime_ids, &runtime_values)?;

        parsed.push(ParsedEnvVarItem {
            key,
            is_encrypted: item.is_encrypted.unwrap_or(true),
            runtime_values,
        });
    }

    Ok(parsed)
}

fn group_env_var_rows(
    rows: Vec<ProjectRuntimeEnvVarRow>,
    runtime_ids: &[Uuid],
) -> Vec<RuntimeEnvVarGroupedResponse> {
    let mut by_key: HashMap<String, RuntimeEnvVarGroupedResponse> = HashMap::new();
    for row in rows {
        let entry = by_key.entry(row.key.clone()).or_insert_with(|| RuntimeEnvVarGroupedResponse {
            key: row.key.clone(),
            is_encrypted: row.is_encrypted,
            values: HashMap::new(),
        });
        entry.values.insert(
            row.runtime_id.to_string(),
            Some(row.stored_value.clone()),
        );
    }

    for entry in by_key.values_mut() {
        for runtime_id in runtime_ids {
            entry
                .values
                .entry(runtime_id.to_string())
                .or_insert(None);
        }
    }

    let mut grouped: Vec<_> = by_key.into_values().collect();
    grouped.sort_by(|a, b| a.key.cmp(&b.key));
    grouped
}

pub async fn handle_list_project_runtime_env_vars(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    query: web::Query<ListRuntimeEnvVarsQuery>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");
    let project_id = path.into_inner();
    let user_id = access_payload.user_id;
    let is_encrypted_filter = query.is_encrypted;

    let list_result = match database::block_on_db(&context.pool, move |conn| {
        let runtime_ids = load_project_runtime_ids(conn, project_id, user_id)?;
        let mut q = project_runtime_env_vars::table
            .inner_join(projects::table.on(project_runtime_env_vars::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_runtime_env_vars::project_id.eq(project_id))
            .into_boxed();
        if let Some(is_encrypted) = is_encrypted_filter {
            q = q.filter(project_runtime_env_vars::is_encrypted.eq(is_encrypted));
        }
        let rows = q
            .select(ProjectRuntimeEnvVarRow::as_select())
            .load::<ProjectRuntimeEnvVarRow>(conn)?;
        let mut runtime_id_list: Vec<Uuid> = runtime_ids.into_iter().collect();
        runtime_id_list.sort();
        Ok((rows, runtime_id_list))
    })
    .await
    {
        Ok(v) => v,
        Err(BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Project not found", json!({})));
        }
        Err(_) => {
            log::error!("[handle_list_project_runtime_env_vars] DB error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let (rows, runtime_ids) = list_result;
    let grouped = group_env_var_rows(rows, &runtime_ids);
    HttpResponse::Ok().json(api_success(
        "Runtime environment variables fetched successfully",
        json!(grouped),
    ))
}

pub async fn handle_upsert_project_runtime_env_vars(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    body: web::Json<UpsertRuntimeEnvVarsBatchBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");
    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    let upsert_result = database::block_on_db(&context.pool, move |conn| {
        let allowed_runtime_ids = load_project_runtime_ids(conn, project_id, user_id)?;
        let items = parse_upsert_items(body.items.clone(), &allowed_runtime_ids).map_err(|code| {
            diesel::result::Error::QueryBuilderError(
                format!("invalid upsert batch: {}", code).into(),
            )
        })?;

        conn.transaction(|conn| {
            let mut invalidations = Vec::new();
            for item in &items {
                for (runtime_id, plaintext) in &item.runtime_values {
                    invalidations.push((*runtime_id, item.key.clone()));

                    let key_row = ensure_runtime_encryption_key(conn, *runtime_id).map_err(|e| {
                        diesel::result::Error::QueryBuilderError(Box::new(e))
                    })?;
                    let runtime_key = runtime_key_from_row(&key_row).map_err(|e| {
                        diesel::result::Error::QueryBuilderError(Box::new(e))
                    })?;
                    let stored_value =
                        prepare_stored_value(&runtime_key, plaintext, item.is_encrypted).map_err(
                            |e: RuntimeSecretsError| diesel::result::Error::QueryBuilderError(Box::new(e)),
                        )?;

                    diesel::insert_into(project_runtime_env_vars::table)
                        .values(&ProjectRuntimeEnvVarInsert {
                            project_id,
                            runtime_id: *runtime_id,
                            key: item.key.clone(),
                            stored_value,
                            is_encrypted: item.is_encrypted,
                        })
                        .on_conflict((
                            project_runtime_env_vars::runtime_id,
                            project_runtime_env_vars::key,
                        ))
                        .do_update()
                        .set((
                            project_runtime_env_vars::stored_value
                                .eq(excluded(project_runtime_env_vars::stored_value)),
                            project_runtime_env_vars::is_encrypted
                                .eq(excluded(project_runtime_env_vars::is_encrypted)),
                            project_runtime_env_vars::updated_at.eq(now),
                        ))
                        .execute(conn)?;
                }
            }
            Ok((
                invalidations,
                items
                    .iter()
                    .map(|i| json!({ "key": i.key, "isEncrypted": i.is_encrypted }))
                    .collect::<Vec<_>>(),
            ))
        })
    })
    .await;

    match upsert_result {
        Ok((invalidations, saved)) => {
            notify_runtime_env_cache_invalidate(
                &context,
                project_id,
                &invalidations,
                "upsert_runtime_env_vars",
            )
            .await;
            HttpResponse::Ok().json(api_success(
                "Runtime environment variables saved successfully",
                json!({ "items": saved }),
            ))
        }
        Err(BlockingDbError::Query(diesel::result::Error::NotFound)) => HttpResponse::NotFound()
            .json(api_error("Project not found", json!({}))),
        Err(BlockingDbError::Query(diesel::result::Error::QueryBuilderError(e)))
            if e.downcast_ref::<RuntimeSecretsError>().is_some() =>
        {
            log::error!("[handle_upsert_project_runtime_env_vars] encryption error");
            HttpResponse::InternalServerError()
                .json(api_error("Failed to secure environment variable", json!({})))
        }
        Err(BlockingDbError::Query(diesel::result::Error::QueryBuilderError(e))) => {
            let msg = e.to_string();
            if msg.contains("invalid upsert batch") {
                HttpResponse::BadRequest().json(api_error(
                    "Invalid runtime environment variable batch",
                    json!({ "detail": msg }),
                ))
            } else {
                log::error!("[handle_upsert_project_runtime_env_vars] DB error");
                HttpResponse::ServiceUnavailable()
                    .json(api_error("Database temporarily unavailable", json!({})))
            }
        }
        Err(_) => {
            log::error!("[handle_upsert_project_runtime_env_vars] DB error");
            HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})))
        }
    }
}

pub async fn handle_delete_project_runtime_env_vars(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    body: web::Json<DeleteRuntimeEnvVarsBatchBody>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");
    let project_id = path.into_inner();
    let user_id = access_payload.user_id;

    if body.keys.is_empty() {
        return HttpResponse::BadRequest().json(api_error("No keys to delete", json!({ "code": "empty_batch" })));
    }

    let mut keys = Vec::with_capacity(body.keys.len());
    let mut seen = HashSet::new();
    for key_raw in &body.keys {
        let key = match normalize_env_key(key_raw) {
            Ok(k) => k,
            Err(code) => {
                return HttpResponse::BadRequest()
                    .json(api_error("Invalid env key", json!({ "code": code })));
            }
        };
        if !seen.insert(key.clone()) {
            return HttpResponse::BadRequest()
                .json(api_error("Duplicate key in batch", json!({ "code": "duplicate_key" })));
        }
        keys.push(key);
    }

    let keys_for_response = keys.clone();

    let delete_result = match database::block_on_db(&context.pool, move |conn| {
        let runtime_ids: Vec<Uuid> = project_runtimes::table
            .inner_join(projects::table.on(project_runtimes::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(projects::id.eq(project_id))
            .select(project_runtimes::id)
            .load(conn)?;

        if runtime_ids.is_empty() {
            return Ok((0usize, Vec::new()));
        }

        let invalidations: Vec<(Uuid, String)> = runtime_ids
            .iter()
            .flat_map(|runtime_id| keys.iter().map(move |key| (*runtime_id, key.clone())))
            .collect();

        let deleted = diesel::delete(
            project_runtime_env_vars::table
                .filter(project_runtime_env_vars::project_id.eq(project_id))
                .filter(project_runtime_env_vars::key.eq_any(&keys))
                .filter(project_runtime_env_vars::runtime_id.eq_any(&runtime_ids)),
        )
        .execute(conn)?;

        Ok((deleted, invalidations))
    })
    .await
    {
        Ok(v) => v,
        Err(_) => {
            log::error!("[handle_delete_project_runtime_env_vars] DB error");
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let (deleted, invalidations) = delete_result;

    if deleted == 0 {
        return HttpResponse::NotFound().json(api_error("Environment variables not found", json!({})));
    }

    notify_runtime_env_cache_invalidate(
        &context,
        project_id,
        &invalidations,
        "delete_runtime_env_vars",
    )
    .await;

    HttpResponse::Ok().json(api_success(
        "Runtime environment variables deleted successfully",
        json!({ "keys": keys_for_response, "deletedRows": deleted }),
    ))
}

pub async fn handle_reveal_project_runtime_env_var(
    context: web::Data<AppContext>,
    req: HttpRequest,
    path: web::Path<(Uuid, String, Uuid)>,
) -> HttpResponse {
    let extensions = req.extensions_mut();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist");
    let (project_id, key_raw, runtime_id) = path.into_inner();
    let user_id = access_payload.user_id;
    let key = match normalize_env_key(&key_raw) {
        Ok(k) => k,
        Err(code) => {
            return HttpResponse::BadRequest().json(api_error("Invalid env key", json!({ "code": code })));
        }
    };
    let key_for_empty = key.clone();
    let runtime_id_for_empty = runtime_id;

    let reveal = database::block_on_db(&context.pool, move |conn| {
        let row: ProjectRuntimeEnvVarRow = project_runtime_env_vars::table
            .inner_join(projects::table.on(project_runtime_env_vars::project_id.eq(projects::id)))
            .filter(projects::owner_user_id.eq(user_id))
            .filter(project_runtime_env_vars::project_id.eq(project_id))
            .filter(project_runtime_env_vars::runtime_id.eq(runtime_id))
            .filter(project_runtime_env_vars::key.eq(&key))
            .select(ProjectRuntimeEnvVarRow::as_select())
            .first(conn)?;

        let value = if row.stored_value.is_empty() {
            Some(String::new())
        } else if row.is_encrypted {
            let key_row = ensure_runtime_encryption_key(conn, runtime_id).map_err(|e| {
                diesel::result::Error::QueryBuilderError(Box::new(e))
            })?;
            let runtime_key = runtime_key_from_row(&key_row).map_err(|e| {
                diesel::result::Error::QueryBuilderError(Box::new(e))
            })?;
            Some(
                runtime_secrets::decrypt_runtime_env_value(&runtime_key, &row.stored_value)
                    .map_err(|e| diesel::result::Error::QueryBuilderError(Box::new(e)))?,
            )
        } else {
            Some(row.stored_value.clone())
        };

        Ok(RuntimeEnvVarRevealResponse {
            key: row.key,
            runtime_id,
            value,
            is_encrypted: row.is_encrypted,
        })
    })
    .await;

    match reveal {
        Ok(payload) => HttpResponse::Ok().json(api_success(
            "Runtime environment variable revealed successfully",
            json!(payload),
        )),
        Err(BlockingDbError::Query(diesel::result::Error::NotFound)) => HttpResponse::Ok().json(
            api_success(
                "Runtime environment variable revealed successfully",
                json!(RuntimeEnvVarRevealResponse {
                    key: key_for_empty,
                    runtime_id: runtime_id_for_empty,
                    value: None,
                    is_encrypted: false,
                }),
            ),
        ),
        Err(BlockingDbError::Query(diesel::result::Error::QueryBuilderError(e)))
            if e.downcast_ref::<RuntimeSecretsError>().is_some() =>
        {
            log::error!("[handle_reveal_project_runtime_env_var] decryption error");
            HttpResponse::InternalServerError()
                .json(api_error("Failed to decrypt environment variable", json!({})))
        }
        Err(_) => {
            log::error!("[handle_reveal_project_runtime_env_var] DB error");
            HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})))
        }
    }
}
