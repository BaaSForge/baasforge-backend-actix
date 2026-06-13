use crate::database;
use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::AccessPayload;
use crate::routes::AppContext;
use crate::services::usage::{
    UsagePeriodFilter, calculate_user_usage_cost, list_project_usage_entries_page,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

fn default_usage_list_page() -> u32 {
    1
}

fn default_usage_list_page_size() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
pub struct UsagePeriodQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct ListUsageEntriesQuery {
    #[serde(default = "default_usage_list_page")]
    pub page: u32,
    #[serde(default = "default_usage_list_page_size")]
    pub page_size: u32,
    pub project_id: Option<Uuid>,
    #[serde(flatten)]
    pub period: UsagePeriodQuery,
}

#[derive(Debug, Deserialize)]
pub struct UsageCostQuery {
    pub project_id: Option<Uuid>,
    #[serde(flatten)]
    pub period: UsagePeriodQuery,
}

fn validate_period(period: &UsagePeriodQuery) -> Option<HttpResponse> {
    let filter = UsagePeriodFilter::new(period.from, period.to);
    filter.validate().err().map(|msg| {
        HttpResponse::BadRequest().json(api_error(msg, json!({})))
    })
}

pub async fn handle_list_usage_entries(
    context: web::Data<AppContext>,
    req: HttpRequest,
    query: web::Query<ListUsageEntriesQuery>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to list usage entries");
    let user_id = access_payload.user_id;
    let q = query.into_inner();

    if let Some(response) = validate_period(&q.period) {
        return response;
    }

    let period = UsagePeriodFilter::new(q.period.from, q.period.to);

    let outcome = match database::block_on_db(&context.pool, move |conn| {
        list_project_usage_entries_page(
            conn,
            user_id,
            q.project_id,
            period.from,
            period.to,
            q.page,
            q.page_size,
        )
    })
    .await
    {
        Ok(v) => v,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Project not found", json!({})));
        }
        Err(e) => {
            log::error!("[handle_list_usage_entries] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    let (entries, total, page, page_size) = outcome;

    HttpResponse::Ok().json(api_success(
        "Usage entries fetched successfully",
        json!({
            "entries": entries,
            "total": total,
            "page": page,
            "page_size": page_size,
            "period": period,
        }),
    ))
}

pub async fn handle_get_usage_cost(
    context: web::Data<AppContext>,
    req: HttpRequest,
    query: web::Query<UsageCostQuery>,
) -> HttpResponse {
    let extensions = req.extensions();
    let access_payload = extensions
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to get usage cost");
    let user_id = access_payload.user_id;
    let q = query.into_inner();

    if let Some(response) = validate_period(&q.period) {
        return response;
    }

    let period = UsagePeriodFilter::new(q.period.from, q.period.to);
    if let Err(msg) = period.validate_for_billing() {
        return HttpResponse::BadRequest().json(api_error(msg, json!({})));
    }

    let from = q.period.from;
    let to = q.period.to;

    let summary = match database::block_on_db(&context.pool, move |conn| {
        calculate_user_usage_cost(conn, user_id, q.project_id, from, to)
    })
    .await
    {
        Ok(v) => v,
        Err(database::BlockingDbError::Query(diesel::result::Error::NotFound)) => {
            return HttpResponse::NotFound().json(api_error("Project not found", json!({})));
        }
        Err(e) => {
            log::error!("[handle_get_usage_cost] DB/block error: {}", e);
            return HttpResponse::ServiceUnavailable()
                .json(api_error("Database temporarily unavailable", json!({})));
        }
    };

    HttpResponse::Ok().json(api_success(
        "Usage cost calculated successfully",
        json!(summary),
    ))
}
