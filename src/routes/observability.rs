use crate::libs::api_response::{api_error, api_success};
use crate::routes::AppContext;
use crate::services::observability::{
    DEFAULT_LIMIT_PER_SERVICE, DEFAULT_LOOKBACK_MINUTES, JaegerQueryError, JaegerQueryFilters,
    MAX_LIMIT_PER_SERVICE, MAX_LOOKBACK_MINUTES, ObservabilityService, query_jaeger_traces,
};
use actix_web::{HttpResponse, web};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JaegerTracesQueryBody {
    /// Optional. When provided, fetches the single trace and groups its spans by service.
    #[serde(default, rename = "traceID")]
    pub trace_id: Option<String>,
    /// Optional. Filters spans whose `baal.release.id` tag matches.
    #[serde(default, rename = "releaseID")]
    pub release_id: Option<String>,
    pub services: Vec<ObservabilityService>,
    /// Optional. Defaults to [`DEFAULT_LOOKBACK_MINUTES`], capped at [`MAX_LOOKBACK_MINUTES`].
    #[serde(default)]
    pub lookback: Option<u32>,
    /// Optional. Defaults to [`DEFAULT_LIMIT_PER_SERVICE`], capped at [`MAX_LIMIT_PER_SERVICE`].
    #[serde(default)]
    pub limit_per_service: Option<usize>,
}

pub async fn handle_query_jaeger_traces(
    context: web::Data<AppContext>,
    body: web::Json<JaegerTracesQueryBody>,
) -> HttpResponse {
    let JaegerTracesQueryBody {
        trace_id,
        release_id,
        services,
        lookback,
        limit_per_service,
    } = body.into_inner();

    let trace_id = trace_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let release_id = release_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if let Some(ref tid) = trace_id
        && !tid.chars().all(|c| c.is_ascii_hexdigit())
    {
        return HttpResponse::BadRequest().json(api_error(
            "traceID must contain only hex characters",
            json!({}),
        ));
    }
    if services.is_empty() {
        return HttpResponse::BadRequest().json(api_error(
            "services must contain at least one service",
            json!({}),
        ));
    }
    let lookback = lookback.unwrap_or(DEFAULT_LOOKBACK_MINUTES);
    if lookback == 0 || lookback > MAX_LOOKBACK_MINUTES {
        return HttpResponse::BadRequest().json(api_error(
            &format!(
                "lookback must be between 1 and {} minutes",
                MAX_LOOKBACK_MINUTES
            ),
            json!({}),
        ));
    }
    let limit_per_service = limit_per_service.unwrap_or(DEFAULT_LIMIT_PER_SERVICE);
    if limit_per_service == 0 || limit_per_service > MAX_LIMIT_PER_SERVICE {
        return HttpResponse::BadRequest().json(api_error(
            &format!(
                "limitPerService must be between 1 and {}",
                MAX_LIMIT_PER_SERVICE
            ),
            json!({}),
        ));
    }

    let filters = JaegerQueryFilters {
        trace_id: trace_id.clone(),
        release_id: release_id.clone(),
        services,
        lookback_minutes: lookback,
        limit_per_service,
    };

    let by_service =
        match query_jaeger_traces(&context.settings.jaeger_query_base_url, &filters).await {
            Ok(v) => v,
            Err(e) => return jaeger_error_to_response(e),
        };

    HttpResponse::Ok().json(api_success(
        "Jaeger traces fetched successfully",
        json!({
            "traceID": trace_id,
            "releaseID": release_id,
            "lookback": lookback,
            "limitPerService": limit_per_service,
            "byService": by_service,
        }),
    ))
}

fn jaeger_error_to_response(err: JaegerQueryError) -> HttpResponse {
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
        JaegerQueryError::InvalidPayload { .. } => HttpResponse::BadGateway()
            .json(api_error("Invalid response from Jaeger", json!({}))),
    }
}
