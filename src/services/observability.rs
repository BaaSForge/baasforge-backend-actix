//! Jaeger Query proxy service.
//!
//! Pure data-loading layer: takes filters (`JaegerQueryFilters`), calls the
//! Jaeger HTTP API, and returns spans grouped by service. The HTTP route in
//! `routes::observability` is a thin wrapper that does request validation and
//! maps errors into HTTP responses.

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// If this value changes, update the user-facing "15 minutes" copy in `chat_worker::agent::planner`
/// (`SYSTEM_PROMPT_CORE`).
pub const MAX_LOOKBACK_MINUTES: u32 = 15;
pub const DEFAULT_LOOKBACK_MINUTES: u32 = MAX_LOOKBACK_MINUTES;
pub const MAX_LIMIT_PER_SERVICE: usize = 20;
pub const DEFAULT_LIMIT_PER_SERVICE: usize = 20;

pub const RELEASE_ID_TAG_KEY: &str = "baal.release.id";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Backend services we accept as a filter. Values match `service.name` reported
/// by each process to OpenTelemetry / Jaeger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservabilityService {
    ApiServer,
    ActorWorker,
    QueueWorkerWorker,
    WorkflowWorker,
}

impl ObservabilityService {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiServer => "api_server",
            Self::ActorWorker => "actor_worker",
            Self::QueueWorkerWorker => "queue_worker_worker",
            Self::WorkflowWorker => "workflow_worker",
        }
    }
}

#[derive(Debug, Clone)]
pub struct JaegerQueryFilters {
    /// When set, fetches the single trace by id; otherwise per-service search.
    pub trace_id: Option<String>,
    /// When set, only spans tagged `baal.release.id == release_id` are kept.
    pub release_id: Option<String>,
    pub services: Vec<ObservabilityService>,
    pub lookback_minutes: u32,
    pub limit_per_service: usize,
}

#[derive(Debug)]
pub enum JaegerQueryError {
    ClientBuild(String),
    Transport {
        url: String,
        message: String,
    },
    BadStatus {
        url: String,
        status: u16,
        body: String,
    },
    InvalidPayload {
        url: String,
        message: String,
    },
}

impl std::fmt::Display for JaegerQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClientBuild(m) => write!(f, "client build: {}", m),
            Self::Transport { url, message } => {
                write!(f, "transport ({}): {}", url, message)
            }
            Self::BadStatus { url, status, body } => {
                write!(f, "bad status {} ({}): {}", status, url, body)
            }
            Self::InvalidPayload { url, message } => {
                write!(f, "invalid payload ({}): {}", url, message)
            }
        }
    }
}

impl std::error::Error for JaegerQueryError {}

/// Returns spans grouped by service name. Every entry in `filters.services` is
/// guaranteed to be present (with an empty `Vec` when nothing matched).
pub async fn query_jaeger_traces(
    base_url: &str,
    filters: &JaegerQueryFilters,
) -> Result<BTreeMap<String, Vec<Value>>, JaegerQueryError> {
    let base = base_url.trim_end_matches('/');
    let limit_per_service = filters.limit_per_service.clamp(1, MAX_LIMIT_PER_SERVICE);

    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let lookback = filters.lookback_minutes.clamp(1, MAX_LOOKBACK_MINUTES);
    let search_start_us = now_us.saturating_sub((lookback as u64) * 60 * 1_000_000);

    let client = build_jaeger_client().map_err(|e| JaegerQueryError::ClientBuild(e.to_string()))?;

    let requested: HashSet<&str> = filters.services.iter().map(|s| s.as_str()).collect();

    let mut by_service: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for s in &filters.services {
        by_service.insert(s.as_str().to_string(), Vec::new());
    }

    let release_id = filters.release_id.as_deref();

    if let Some(tid) = filters.trace_id.as_deref() {
        let url = format!(
            "{}/api/traces/{}?start={}&end={}",
            base,
            urlencoding::encode(tid),
            0u64,
            now_us,
        );
        let payload = fetch_jaeger_json(&client, &url).await?;
        merge_spans_into_buckets(
            &payload,
            &requested,
            release_id,
            limit_per_service,
            &mut by_service,
        );
    } else {
        let release_tags_param = release_id.map(|rid| {
            let tags_json = json!({ RELEASE_ID_TAG_KEY: rid }).to_string();
            urlencoding::encode(&tags_json).to_string()
        });

        let urls: Vec<(String, String)> = filters
            .services
            .iter()
            .map(|svc| {
                let mut url = format!(
                    "{}/api/traces?service={}&start={}&end={}&limit={}",
                    base,
                    urlencoding::encode(svc.as_str()),
                    search_start_us,
                    now_us,
                    limit_per_service,
                );
                if let Some(ref encoded) = release_tags_param {
                    url.push_str(&format!("&tags={}", encoded));
                }
                (svc.as_str().to_string(), url)
            })
            .collect();

        let results = join_all(urls.iter().map(|(_, url)| fetch_jaeger_json(&client, url))).await;

        for ((svc_name, _), result) in urls.iter().zip(results) {
            let payload = result?;
            let mut single = HashSet::new();
            single.insert(svc_name.as_str());
            let mut local: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            local.insert(svc_name.clone(), Vec::new());
            merge_spans_into_buckets(&payload, &single, release_id, limit_per_service, &mut local);
            if let Some(spans) = local.remove(svc_name) {
                by_service.insert(svc_name.clone(), spans);
            }
        }
    }

    Ok(by_service)
}

fn build_jaeger_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .http1_only()
        .pool_max_idle_per_host(0)
        .no_proxy()
        .timeout(REQUEST_TIMEOUT)
        .build()
}

async fn fetch_jaeger_json(client: &reqwest::Client, url: &str) -> Result<Value, JaegerQueryError> {
    let response = client.get(url).send().await.map_err(|e| {
        let chain = error_chain(&e);
        log::error!(
            "[observability] Jaeger request failed url={} err={} chain={}",
            url,
            e,
            chain,
        );
        JaegerQueryError::Transport {
            url: url.to_string(),
            message: format!("{} ({})", e, chain),
        }
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        log::warn!(
            "[observability] Jaeger error url={} status={} body={}",
            url,
            status,
            body
        );
        return Err(JaegerQueryError::BadStatus {
            url: url.to_string(),
            status: status.as_u16(),
            body,
        });
    }

    response.json::<Value>().await.map_err(|e| {
        let chain = error_chain(&e);
        log::error!(
            "[observability] Failed to parse Jaeger JSON url={} err={} chain={}",
            url,
            e,
            chain,
        );
        JaegerQueryError::InvalidPayload {
            url: url.to_string(),
            message: format!("{} ({})", e, chain),
        }
    })
}

fn merge_spans_into_buckets(
    payload: &Value,
    requested: &HashSet<&str>,
    release_id: Option<&str>,
    limit_per_service: usize,
    by_service: &mut BTreeMap<String, Vec<Value>>,
) {
    let Some(data) = payload.get("data").and_then(|v| v.as_array()) else {
        return;
    };

    for trace in data {
        let processes = trace.get("processes").and_then(|v| v.as_object());
        let spans = trace.get("spans").and_then(|v| v.as_array());
        let (Some(processes), Some(spans)) = (processes, spans) else {
            continue;
        };

        let proc_to_service: HashMap<String, String> = processes
            .iter()
            .filter_map(|(pid, p)| {
                p.get("serviceName")
                    .and_then(|s| s.as_str())
                    .map(|s| (pid.clone(), s.to_string()))
            })
            .collect();

        let mut grouped: HashMap<String, Vec<&Value>> = HashMap::new();
        for span in spans {
            let Some(pid) = span.get("processID").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(svc) = proc_to_service.get(pid) else {
                continue;
            };
            if !requested.contains(svc.as_str()) {
                continue;
            }
            if let Some(rid) = release_id
                && !span_has_tag(span, RELEASE_ID_TAG_KEY, rid)
            {
                continue;
            }
            grouped.entry(svc.clone()).or_default().push(span);
        }

        for (svc, mut spans_for_svc) in grouped {
            spans_for_svc.sort_by_key(|s| s.get("startTime").and_then(|v| v.as_u64()).unwrap_or(0));
            let bucket = by_service.entry(svc).or_default();
            for span in spans_for_svc {
                if bucket.len() >= limit_per_service {
                    break;
                }
                bucket.push(span.clone());
            }
        }
    }
}

fn span_has_tag(span: &Value, key: &str, val: &str) -> bool {
    let Some(tags) = span.get("tags").and_then(|t| t.as_array()) else {
        return false;
    };
    tags.iter().any(|t| {
        let matches_key = t.get("key").and_then(|k| k.as_str()) == Some(key);
        if !matches_key {
            return false;
        }
        match t.get("value") {
            Some(Value::String(s)) => s == val,
            Some(other) => other.to_string().trim_matches('"') == val,
            None => false,
        }
    })
}

fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = String::new();
    let mut current: Option<&(dyn std::error::Error + 'static)> = err.source();
    while let Some(e) = current {
        if !out.is_empty() {
            out.push_str(" -> ");
        }
        out.push_str(&e.to_string());
        current = e.source();
    }
    if out.is_empty() {
        "<no source>".to_string()
    } else {
        out
    }
}
