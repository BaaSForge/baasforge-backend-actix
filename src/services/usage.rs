use crate::models::project_usage_entries::ProjectUsageEntryResponse;
use crate::schema::{project_usage_entries, projects};
use crate::services::billing::{period_total, validate_billing_period, PeriodBilling};
use crate::services::payment::{package_tier_at, package_tier_segments};
use crate::services::pricing::billing_rates_for_package_tier;
use crate::services::pricing::SUBSCRIPTION_METRIC;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::Serialize;
use uuid::Uuid;

const MAX_PAGE_SIZE: u32 = 100;

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
        count_query = count_query
            .filter(project_usage_entries::project_id.eq(project_id))
            .filter(project_usage_entries::metric.ne(SUBSCRIPTION_METRIC));
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
        list_query = list_query
            .filter(project_usage_entries::project_id.eq(project_id))
            .filter(project_usage_entries::metric.ne(SUBSCRIPTION_METRIC));
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

    pub fn validate_for_billing(&self) -> Result<(DateTime<Utc>, DateTime<Utc>), &'static str> {
        let (from, to) = match (self.from, self.to) {
            (Some(from), Some(to)) => (from, to),
            _ => return Err("`from` and `to` are required for billing"),
        };
        validate_billing_period(from, to)?;
        Ok((from, to))
    }
}

#[derive(Debug, Serialize)]
pub struct UsageCostSummary {
    pub package_tier: i32,
    pub period: UsagePeriodFilter,
    pub sum_gb_ms: i64,
    pub compute_cost: f64,
    pub byte_hours_active: f64,
    pub byte_hours_retained: f64,
    pub storage_hot_cost: f64,
    pub storage_cold_cost: f64,
    pub sum_input_tokens: i64,
    pub sum_output_tokens: i64,
    pub llm_input_cost: f64,
    pub llm_output_cost: f64,
    pub subscription_entity_seconds: i64,
    pub project_entity_seconds: i64,
    pub unit_entity_seconds: i64,
    pub stage_runtime_entity_seconds: i64,
    pub subscription_cost: f64,
    pub project_cost: f64,
    pub unit_cost: f64,
    pub stage_runtime_cost: f64,
    pub total_cost: f64,
}

impl UsageCostSummary {
    fn from_billing(
        package_tier: i32,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        billing: PeriodBilling,
    ) -> Self {
        Self {
            package_tier,
            period: UsagePeriodFilter::new(Some(from), Some(to)),
            sum_gb_ms: billing.sum_gb_ms,
            compute_cost: billing.compute_cost,
            byte_hours_active: billing.byte_hours_active,
            byte_hours_retained: billing.byte_hours_retained,
            storage_hot_cost: billing.storage_hot_cost,
            storage_cold_cost: billing.storage_cold_cost,
            sum_input_tokens: billing.sum_input_tokens,
            sum_output_tokens: billing.sum_output_tokens,
            llm_input_cost: billing.llm_input_cost,
            llm_output_cost: billing.llm_output_cost,
            subscription_entity_seconds: billing.platform.subscription_entity_seconds,
            project_entity_seconds: billing.platform.project_entity_seconds,
            unit_entity_seconds: billing.platform.unit_entity_seconds,
            stage_runtime_entity_seconds: billing.platform.stage_runtime_entity_seconds,
            subscription_cost: billing.platform.subscription_cost,
            project_cost: billing.platform.project_cost,
            unit_cost: billing.platform.unit_cost,
            stage_runtime_cost: billing.platform.stage_runtime_cost,
            total_cost: billing.total,
        }
    }
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
    let period = UsagePeriodFilter::new(from, to);
    let (from, to) = period
        .validate_for_billing()
        .expect("billing period must be validated before calling calculate_user_usage_cost");

    if let Some(project_id) = project_id {
        ensure_owned_project(conn, user_id, project_id)?;
    }

    let segments = package_tier_segments(conn, user_id, from, to)?;
    let mut billing = PeriodBilling::zero();
    for (seg_from, seg_to, tier) in segments {
        let rates = billing_rates_for_package_tier(tier);
        let segment_billing =
            period_total(conn, user_id, project_id, seg_from, seg_to, rates, tier)?;
        billing.merge(&segment_billing);
    }

    let package_tier = package_tier_at(conn, user_id, to)?;
    Ok(UsageCostSummary::from_billing(
        package_tier,
        from,
        to,
        billing,
    ))
}

pub fn calculate_user_usage_cost_between<C>(
    conn: &mut C,
    user_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<f64, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    use crate::services::billing::MAX_BILLING_PERIOD_DAYS;
    use chrono::Duration;

    if from >= to {
        return Ok(0.0);
    }

    let chunk = Duration::days(MAX_BILLING_PERIOD_DAYS);
    let mut cursor = from;
    let mut total = 0.0;
    while cursor < to {
        let next = std::cmp::min(cursor + chunk, to);
        let summary =
            calculate_user_usage_cost(conn, user_id, None, Some(cursor), Some(next))?;
        total += summary.total_cost;
        cursor = next;
    }

    Ok(total)
}

pub fn calculate_user_overall_usage_cost<C>(
    conn: &mut C,
    user_id: Uuid,
) -> Result<f64, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    use diesel::dsl::min;

    let earliest = project_usage_entries::table
        .inner_join(projects::table.on(project_usage_entries::project_id.eq(projects::id)))
        .filter(projects::owner_user_id.eq(user_id))
        .select(min(project_usage_entries::recorded_at))
        .first::<Option<DateTime<Utc>>>(conn)?;

    let Some(from) = earliest else {
        return Ok(0.0);
    };
    calculate_user_usage_cost_between(conn, user_id, from, Utc::now())
}
