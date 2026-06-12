use crate::models::project_usage_entries::ProjectUsageEntryResponse;
use crate::schema::{project_usage_entries, projects};
use crate::services::payment::get_user_active_paid_packages;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

const MAX_PAGE_SIZE: u32 = 100;

fn effective_tier(tier: i32) -> i32 {
    if tier <= 0 { 1 } else { tier }
}

fn tier_rates(tier: i32) -> &'static HashMap<&'static str, f64> {
    static TIER_1: std::sync::OnceLock<HashMap<&'static str, f64>> = std::sync::OnceLock::new();
    static TIER_2: std::sync::OnceLock<HashMap<&'static str, f64>> = std::sync::OnceLock::new();
    static TIER_3: std::sync::OnceLock<HashMap<&'static str, f64>> = std::sync::OnceLock::new();

    fn base_rates() -> HashMap<&'static str, f64> {
        HashMap::from([
            ("compute.gb_ms", 1.0e-9),
            ("compute.actions", 1.0e-4),
            ("storage.bytes", 3.0e-12),
            ("llm.input_tokens", 2.0e-6),
            ("llm.output_tokens", 8.0e-6),
        ])
    }

    match effective_tier(tier) {
        1 => TIER_1.get_or_init(base_rates),
        2 => TIER_2.get_or_init(|| {
            let mut m = base_rates();
            m.insert("compute.gb_ms", 8.0e-10);
            m.insert("compute.actions", 8.0e-5);
            m.insert("storage.bytes", 2.4e-12);
            m.insert("llm.input_tokens", 1.6e-6);
            m.insert("llm.output_tokens", 6.4e-6);
            m
        }),
        _ => TIER_3.get_or_init(|| {
            let mut m = base_rates();
            m.insert("compute.gb_ms", 6.0e-10);
            m.insert("compute.actions", 6.0e-5);
            m.insert("storage.bytes", 1.8e-12);
            m.insert("llm.input_tokens", 1.2e-6);
            m.insert("llm.output_tokens", 4.8e-6);
            m
        }),
    }
}

pub fn rate_for_metric(tier: i32, kind: &str, metric: &str) -> f64 {
    let rates = tier_rates(tier);
    rates
        .get(metric)
        .copied()
        .or_else(|| {
            if kind == "storage" {
                rates.get("storage.bytes").copied()
            } else {
                None
            }
        })
        .unwrap_or(0.0)
}

fn storage_quantity_sign(quantity: i64, details: &Value) -> i64 {
    let sign = match details.get("op").and_then(|v| v.as_str()) {
        Some("decr") => -1,
        _ => 1,
    };
    quantity.saturating_mul(sign)
}

pub fn line_cost(tier: i32, kind: &str, metric: &str, quantity: i64, details: &Value) -> f64 {
    let rate = rate_for_metric(tier, kind, metric);
    let billable_quantity = if kind == "storage" {
        storage_quantity_sign(quantity, details)
    } else {
        quantity
    };
    billable_quantity as f64 * rate
}

pub fn ensure_owned_project<C>(
    conn: &mut C,
    user_id: Uuid,
    project_id: Uuid,
) -> Result<(), diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    projects::table
        .filter(projects::owner_user_id.eq(user_id))
        .filter(projects::id.eq(project_id))
        .select(projects::id)
        .first::<Uuid>(conn)?;
    Ok(())
}

pub fn list_project_usage_entries_page<C>(
    conn: &mut C,
    user_id: Uuid,
    project_id: Option<Uuid>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    page: u32,
    page_size: u32,
) -> Result<(Vec<ProjectUsageEntryResponse>, i64, u32, u32), diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    if let Some(project_id) = project_id {
        ensure_owned_project(conn, user_id, project_id)?;
    }

    let page = page.max(1);
    let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
    let offset = (page.saturating_sub(1) as i64).saturating_mul(page_size as i64);

    let mut count_query = project_usage_entries::table
        .inner_join(projects::table.on(project_usage_entries::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .into_boxed();

    if let Some(project_id) = project_id {
        count_query = count_query.filter(project_usage_entries::project_id.eq(project_id));
    }
    if let Some(from) = from {
        count_query = count_query.filter(project_usage_entries::recorded_at.ge(from));
    }
    if let Some(to) = to {
        count_query = count_query.filter(project_usage_entries::recorded_at.lt(to));
    }

    let total = count_query.count().get_result::<i64>(conn)?;

    let mut list_query = project_usage_entries::table
        .inner_join(projects::table.on(project_usage_entries::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .into_boxed();

    if let Some(project_id) = project_id {
        list_query = list_query.filter(project_usage_entries::project_id.eq(project_id));
    }
    if let Some(from) = from {
        list_query = list_query.filter(project_usage_entries::recorded_at.ge(from));
    }
    if let Some(to) = to {
        list_query = list_query.filter(project_usage_entries::recorded_at.lt(to));
    }

    let entries = list_query
        .order(project_usage_entries::recorded_at.desc())
        .limit(page_size as i64)
        .offset(offset)
        .select(ProjectUsageEntryResponse::as_select())
        .load::<ProjectUsageEntryResponse>(conn)?;

    Ok((entries, total, page, page_size))
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct UsagePeriodFilter {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl UsagePeriodFilter {
    pub fn new(from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> Self {
        Self { from, to }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if let (Some(from), Some(to)) = (self.from, self.to) {
            if from >= to {
                return Err("`from` must be before `to`");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct UsageCostKindBreakdown {
    pub kind: String,
    pub cost: f64,
}

#[derive(Debug, Serialize)]
pub struct UsageCostSummary {
    pub package_tier: i32,
    pub period: UsagePeriodFilter,
    pub breakdown: Vec<UsageCostKindBreakdown>,
    pub total_cost: f64,
}

pub fn calculate_user_usage_cost<C>(
    conn: &mut C,
    user_id: Uuid,
    project_id: Option<Uuid>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<UsageCostSummary, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    if let Some(project_id) = project_id {
        ensure_owned_project(conn, user_id, project_id)?;
    }

    let (_, largest_tier) = get_user_active_paid_packages(conn, user_id)?;
    let tier = effective_tier(largest_tier);

    let mut query = project_usage_entries::table
        .inner_join(projects::table.on(project_usage_entries::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .into_boxed();

    if let Some(project_id) = project_id {
        query = query.filter(project_usage_entries::project_id.eq(project_id));
    }
    if let Some(from) = from {
        query = query.filter(project_usage_entries::recorded_at.ge(from));
    }
    if let Some(to) = to {
        query = query.filter(project_usage_entries::recorded_at.lt(to));
    }

    let rows = query
        .select((
            project_usage_entries::kind,
            project_usage_entries::metric,
            project_usage_entries::quantity,
            project_usage_entries::details,
        ))
        .load::<(String, String, i64, Value)>(conn)?;

    let mut by_kind: HashMap<String, f64> = HashMap::new();
    let mut total_cost = 0.0_f64;

    for (kind, metric, quantity, details) in rows {
        let cost = line_cost(tier, &kind, &metric, quantity, &details);
        total_cost += cost;
        *by_kind.entry(kind).or_insert(0.0) += cost;
    }

    let mut breakdown: Vec<UsageCostKindBreakdown> = by_kind
        .into_iter()
        .map(|(kind, cost)| UsageCostKindBreakdown { kind, cost })
        .collect();
    breakdown.sort_by(|a, b| a.kind.cmp(&b.kind));

    Ok(UsageCostSummary {
        package_tier: tier,
        period: UsagePeriodFilter::new(from, to),
        breakdown,
        total_cost,
    })
}
