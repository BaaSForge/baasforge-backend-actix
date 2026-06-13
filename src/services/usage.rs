use crate::models::project_usage_entries::ProjectUsageEntryResponse;
use crate::schema::{project_usage_entries, projects};
use crate::services::billing::{
    billing_rates_for_package_tier, period_total, validate_billing_period, PeriodBilling,
};
use crate::services::payment::get_user_active_paid_packages;
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

    let (_, largest_tier) = get_user_active_paid_packages(conn, user_id)?;
    let rates = billing_rates_for_package_tier(largest_tier);
    let billing = period_total(conn, user_id, project_id, from, to, rates)?;

    Ok(UsageCostSummary::from_billing(
        if largest_tier <= 0 {
            1
        } else {
            largest_tier
        },
        from,
        to,
        billing,
    ))
}
