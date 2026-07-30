use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Text, Timestamptz, Uuid as SqlUuid};
use diesel::QueryableByName;
use uuid::Uuid;

use crate::services::pricing::{self, compute_llm_period_cost, LlmPeriodCost, LlmUsageSlice};
use crate::services::llm_usage::{log_llm_usage, LlmUsageRecord};

pub const MAX_BILLING_PERIOD_DAYS: i64 = 62;

pub use pricing::{
    billing_rates_for_package_tier, platform_rates_for_package_tier, BillingRates,
    PlatformResourceRates, COMPUTE_GB_MS_METRIC, ENTITY_SECOND_PRICE_UNIT, LLM_INPUT_TOKENS_METRIC,
    LLM_KIND, LLM_OUTPUT_TOKENS_METRIC, PLATFORM_KIND, PROJECT_METRIC, STAGE_RUNTIME_METRIC,
    STORAGE_TIER_ACTIVE, STORAGE_TIER_RETAINED, SUBSCRIPTION_METRIC, TOKEN_PRICE_UNIT, UNIT_METRIC,
};

#[derive(Debug, Clone)]
pub struct LlmUsageLogContext {
    pub model: String,
    pub provider: String,
    pub message_id: Uuid,
    pub chat_session_id: Uuid,
    pub user_id: Uuid,
}

#[derive(Debug, Clone, Default)]
pub struct LlmTokenMeter {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
}

impl LlmTokenMeter {
    pub fn record_openai_usage(
        &mut self,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
    ) {
        if let Some(n) = prompt_tokens {
            self.input_tokens = Some(n as i64);
        }
        if let Some(n) = completion_tokens {
            self.output_tokens = Some(n as i64);
        }
    }

    pub fn record_claude_input_tokens(&mut self, input_tokens: u64) {
        if input_tokens > 0 {
            self.input_tokens = Some(input_tokens as i64);
        }
    }

    pub fn record_claude_output_tokens(&mut self, output_tokens: u64) {
        if output_tokens > 0 {
            self.output_tokens = Some(output_tokens as i64);
        }
    }

    pub fn record_claude_input_tokens_if_present(&mut self, input_tokens: Option<u64>) {
        if let Some(n) = input_tokens.filter(|&n| n > 0) {
            self.input_tokens = Some(n as i64);
        }
    }

    pub fn record_gemini_usage(
        &mut self,
        prompt_tokens: i32,
        candidates_tokens: i32,
        thoughts_tokens: Option<i32>,
        cached_tokens: Option<i32>,
    ) {
        self.input_tokens = Some(prompt_tokens as i64);
        self.output_tokens = Some(candidates_tokens as i64);
        self.reasoning_tokens = thoughts_tokens.map(|n| n as i64);
        self.cached_tokens = cached_tokens.map(|n| n as i64);
    }

    fn has_billable_tokens(&self) -> bool {
        self.input_tokens.is_some_and(|n| n > 0)
            || self.output_tokens.is_some_and(|n| n > 0)
    }

    pub fn log_entries(&self, project_id: Uuid, ctx: &LlmUsageLogContext) {
        if !self.has_billable_tokens() {
            return;
        }

        log_llm_usage(LlmUsageRecord {
            project_id,
            model: ctx.model.clone(),
            provider: ctx.provider.clone(),
            input_tokens: self.input_tokens.unwrap_or(0).max(0) as u64,
            output_tokens: self.output_tokens.unwrap_or(0).max(0) as u64,
            request_id: Some(ctx.message_id.to_string()),
            chat_session_id: Some(ctx.chat_session_id),
            message_id: Some(ctx.message_id),
            user_id: Some(ctx.user_id),
            cached_tokens: self.cached_tokens,
            reasoning_tokens: self.reasoning_tokens,
        });
    }
}

// TODO: daily balance snapshots / project_usage_daily_totals for faster billing

pub fn validate_billing_period(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(), &'static str> {
    if start >= end {
        return Err("`from` must be before `to`");
    }
    let max = chrono::Duration::days(MAX_BILLING_PERIOD_DAYS);
    if end - start > max {
        return Err("billing period must not exceed 2 months");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMeterEvent {
    pub id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub tier: String,
    pub op: String,
    pub quantity: i64,
}

impl StorageMeterEvent {
    fn signed_quantity(&self) -> i64 {
        if self.op == "decr" {
            -self.quantity
        } else {
            self.quantity
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputeMeterEvent {
    pub recorded_at: DateTime<Utc>,
    pub metric: String,
    pub quantity: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmMeterEvent {
    pub recorded_at: DateTime<Utc>,
    pub provider: String,
    pub model: String,
    pub metric: String,
    pub quantity: i64,
}

pub mod LlmBilling {
    use super::*;

    pub fn sum(events: &[LlmMeterEvent], metric: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
        events
            .iter()
            .filter(|e| {
                e.recorded_at >= start
                    && e.recorded_at < end
                    && e.metric == metric
            })
            .map(|e| e.quantity)
            .sum()
    }

    pub fn period_cost(
        events: &[LlmMeterEvent],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        package_tier: i32,
    ) -> LlmPeriodCost {
        let slices: Vec<LlmUsageSlice> = events
            .iter()
            .filter(|e| e.recorded_at >= start && e.recorded_at < end)
            .map(|e| LlmUsageSlice {
                provider: e.provider.clone(),
                model: e.model.clone(),
                metric: e.metric.clone(),
                quantity: e.quantity,
            })
            .collect();
        compute_llm_period_cost(&slices, package_tier)
    }
}

pub mod ComputeBilling {
    use super::*;

    pub fn sum(events: &[ComputeMeterEvent], start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
        events
            .iter()
            .filter(|e| {
                e.recorded_at >= start
                    && e.recorded_at < end
                    && e.metric == COMPUTE_GB_MS_METRIC
            })
            .map(|e| e.quantity)
            .sum()
    }
}

pub mod StorageBilling {
    use super::*;

    pub fn byte_hours(
        events: &[StorageMeterEvent],
        tier: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> f64 {
        let mut relevant: Vec<&StorageMeterEvent> = events
            .iter()
            .filter(|e| e.tier == tier && e.recorded_at < end)
            .collect();
        relevant.sort_by(|a, b| {
            a.recorded_at
                .cmp(&b.recorded_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut balance: i64 = 0;
        for event in relevant.iter().filter(|e| e.recorded_at < start) {
            balance += event.signed_quantity();
        }

        let in_period: Vec<&StorageMeterEvent> = relevant
            .into_iter()
            .filter(|e| e.recorded_at >= start)
            .collect();

        let mut current = start;
        let mut byte_hours = 0.0_f64;
        let mut i = 0;

        while i < in_period.len() {
            let t = in_period[i].recorded_at;
            if t > current {
                byte_hours +=
                    billable_balance(balance) as f64 * hours_between(current, t);
            }
            while i < in_period.len() && in_period[i].recorded_at == t {
                balance += in_period[i].signed_quantity();
                i += 1;
            }
            current = t;
        }

        if end > current {
            byte_hours += billable_balance(balance) as f64 * hours_between(current, end);
        }

        byte_hours
    }
}

fn billable_balance(balance: i64) -> i64 {
    balance.max(0)
}

fn hours_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    (to - from).num_seconds() as f64 / 3600.0
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlatformPeriodBilling {
    pub subscription_entity_seconds: i64,
    pub project_entity_seconds: i64,
    pub unit_entity_seconds: i64,
    pub stage_runtime_entity_seconds: i64,
    pub subscription_cost: f64,
    pub project_cost: f64,
    pub unit_cost: f64,
    pub stage_runtime_cost: f64,
}

impl PlatformPeriodBilling {
    pub fn total_cost(&self) -> f64 {
        self.subscription_cost + self.project_cost + self.unit_cost + self.stage_runtime_cost
    }

    pub fn from_entity_seconds(rates: PlatformResourceRates, sums: &[(String, i64)]) -> Self {
        let mut subscription_entity_seconds = 0_i64;
        let mut project_entity_seconds = 0_i64;
        let mut unit_entity_seconds = 0_i64;
        let mut stage_runtime_entity_seconds = 0_i64;

        for (metric, quantity) in sums {
            match metric.as_str() {
                SUBSCRIPTION_METRIC => subscription_entity_seconds += quantity,
                PROJECT_METRIC => project_entity_seconds += quantity,
                UNIT_METRIC => unit_entity_seconds += quantity,
                STAGE_RUNTIME_METRIC => stage_runtime_entity_seconds += quantity,
                _ => {}
            }
        }

        Self {
            subscription_entity_seconds,
            project_entity_seconds,
            unit_entity_seconds,
            stage_runtime_entity_seconds,
            subscription_cost: rates.cost_for_entity_seconds(
                SUBSCRIPTION_METRIC,
                subscription_entity_seconds,
            ),
            project_cost: rates.cost_for_entity_seconds(PROJECT_METRIC, project_entity_seconds),
            unit_cost: rates.cost_for_entity_seconds(UNIT_METRIC, unit_entity_seconds),
            stage_runtime_cost: rates.cost_for_entity_seconds(
                STAGE_RUNTIME_METRIC,
                stage_runtime_entity_seconds,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeriodBilling {
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
    pub platform: PlatformPeriodBilling,
    pub total: f64,
}

impl PeriodBilling {
    pub fn from_metered(
        rates: BillingRates,
        sum_gb_ms: i64,
        byte_hours_active: f64,
        byte_hours_retained: f64,
        llm: LlmPeriodCost,
        platform: PlatformPeriodBilling,
    ) -> Self {
        let compute_cost = sum_gb_ms as f64 * rates.compute_gb_ms;
        let storage_hot_cost = rates.storage_active_cost(byte_hours_active);
        let storage_cold_cost = rates.storage_retained_cost(byte_hours_retained);
        let platform_total = platform.total_cost();
        Self {
            sum_gb_ms,
            compute_cost,
            byte_hours_active,
            byte_hours_retained,
            storage_hot_cost,
            storage_cold_cost,
            sum_input_tokens: llm.sum_input_tokens,
            sum_output_tokens: llm.sum_output_tokens,
            llm_input_cost: llm.input_cost,
            llm_output_cost: llm.output_cost,
            platform,
            total: compute_cost
                + storage_hot_cost
                + storage_cold_cost
                + llm.total_cost()
                + platform_total,
        }
    }
}

const COMPUTE_SUM_SQL: &str = "
SELECT COALESCE(SUM(pue.quantity), 0)::bigint AS sum_gb_ms
FROM project_usage_entries pue
INNER JOIN projects pr ON pr.id = pue.project_id
WHERE pr.owner_user_id = $1
  AND ($2::uuid IS NULL OR pue.project_id = $2)
  AND pue.kind = 'compute'
  AND pue.metric = 'compute.gb_ms'
  AND pue.recorded_at >= $3
  AND pue.recorded_at < $4
";

const LLM_USAGE_BY_MODEL_SQL: &str = "
SELECT
  COALESCE(pue.details->>'provider', '') AS provider,
  COALESCE(pue.details->>'model', '') AS model,
  pue.metric AS metric,
  COALESCE(SUM(pue.quantity), 0)::bigint AS quantity
FROM project_usage_entries pue
INNER JOIN projects pr ON pr.id = pue.project_id
WHERE pr.owner_user_id = $1
  AND ($2::uuid IS NULL OR pue.project_id = $2)
  AND pue.kind = 'llm'
  AND pue.metric IN ('input_tokens', 'output_tokens')
  AND pue.recorded_at >= $3
  AND pue.recorded_at < $4
GROUP BY provider, model, metric
";

const STORAGE_EVENTS_SQL: &str = "
SELECT pue.id, pue.recorded_at,
       pue.details->>'tier' AS tier,
       pue.details->>'op' AS op,
       pue.quantity
FROM project_usage_entries pue
INNER JOIN projects pr ON pr.id = pue.project_id
WHERE pr.owner_user_id = $1
  AND ($2::uuid IS NULL OR pue.project_id = $2)
  AND pue.kind = 'storage'
  AND pue.details->>'tier' IN ('active', 'retained')
  AND pue.recorded_at < $3
ORDER BY pue.recorded_at, pue.id
";

const PLATFORM_SUM_SQL: &str = "
SELECT pue.metric AS metric, COALESCE(SUM(pue.quantity), 0)::bigint AS quantity
FROM project_usage_entries pue
INNER JOIN projects pr ON pr.id = pue.project_id
WHERE pr.owner_user_id = $1
  AND ($2::uuid IS NULL OR pue.project_id = $2)
  AND pue.kind = 'platform'
  AND pue.metric IN ('project', 'unit', 'stage_runtime')
  AND pue.recorded_at >= $3
  AND pue.recorded_at < $4
GROUP BY pue.metric
";

const PLATFORM_SUBSCRIPTION_USER_SUM_SQL: &str = "
SELECT COALESCE(SUM(pue.quantity), 0)::bigint AS quantity
FROM project_usage_entries pue
INNER JOIN projects pr ON pr.id = pue.project_id
WHERE pr.owner_user_id = $1
  AND pue.kind = 'platform'
  AND pue.metric = 'subscription'
  AND pue.recorded_at >= $2
  AND pue.recorded_at < $3
";

#[derive(QueryableByName)]
struct PlatformSumRow {
    #[diesel(sql_type = Text)]
    metric: String,
    #[diesel(sql_type = BigInt)]
    quantity: i64,
}

#[derive(QueryableByName)]
struct PlatformSubscriptionSumRow {
    #[diesel(sql_type = BigInt)]
    quantity: i64,
}

#[derive(QueryableByName)]
struct ComputeSumRow {
    #[diesel(sql_type = BigInt)]
    sum_gb_ms: i64,
}

#[derive(QueryableByName)]
struct LlmUsageByModelRow {
    #[diesel(sql_type = Text)]
    provider: String,
    #[diesel(sql_type = Text)]
    model: String,
    #[diesel(sql_type = Text)]
    metric: String,
    #[diesel(sql_type = BigInt)]
    quantity: i64,
}

#[derive(QueryableByName)]
struct StorageEventRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
    #[diesel(sql_type = Timestamptz)]
    recorded_at: DateTime<Utc>,
    #[diesel(sql_type = Text)]
    tier: String,
    #[diesel(sql_type = Text)]
    op: String,
    #[diesel(sql_type = BigInt)]
    quantity: i64,
}

pub fn period_total<C>(
    conn: &mut C,
    user_id: Uuid,
    project_id: Option<Uuid>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    rates: BillingRates,
    package_tier: i32,
) -> Result<PeriodBilling, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let compute_row = sql_query(COMPUTE_SUM_SQL)
        .bind::<SqlUuid, _>(user_id)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(project_id)
        .bind::<Timestamptz, _>(start)
        .bind::<Timestamptz, _>(end)
        .get_result::<ComputeSumRow>(conn)?;

    let llm_rows = sql_query(LLM_USAGE_BY_MODEL_SQL)
        .bind::<SqlUuid, _>(user_id)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(project_id)
        .bind::<Timestamptz, _>(start)
        .bind::<Timestamptz, _>(end)
        .load::<LlmUsageByModelRow>(conn)?;

    let llm_slices: Vec<LlmUsageSlice> = llm_rows
        .into_iter()
        .map(|row| LlmUsageSlice {
            provider: row.provider,
            model: row.model,
            metric: row.metric,
            quantity: row.quantity,
        })
        .collect();
    let llm = compute_llm_period_cost(&llm_slices, package_tier);

    let storage_rows = sql_query(STORAGE_EVENTS_SQL)
        .bind::<SqlUuid, _>(user_id)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(project_id)
        .bind::<Timestamptz, _>(end)
        .load::<StorageEventRow>(conn)?;

    let storage_events: Vec<StorageMeterEvent> = storage_rows
        .into_iter()
        .map(|row| StorageMeterEvent {
            id: row.id,
            recorded_at: row.recorded_at,
            tier: row.tier,
            op: row.op,
            quantity: row.quantity,
        })
        .collect();

    let byte_hours_active =
        StorageBilling::byte_hours(&storage_events, STORAGE_TIER_ACTIVE, start, end);
    let byte_hours_retained =
        StorageBilling::byte_hours(&storage_events, STORAGE_TIER_RETAINED, start, end);

    let platform_rows = sql_query(PLATFORM_SUM_SQL)
        .bind::<SqlUuid, _>(user_id)
        .bind::<diesel::sql_types::Nullable<SqlUuid>, _>(project_id)
        .bind::<Timestamptz, _>(start)
        .bind::<Timestamptz, _>(end)
        .load::<PlatformSumRow>(conn)?;
    let mut platform_sums: Vec<(String, i64)> = platform_rows
        .into_iter()
        .map(|row| (row.metric, row.quantity))
        .collect();

    if project_id.is_none() {
        let subscription_entity_seconds = sql_query(PLATFORM_SUBSCRIPTION_USER_SUM_SQL)
            .bind::<SqlUuid, _>(user_id)
            .bind::<Timestamptz, _>(start)
            .bind::<Timestamptz, _>(end)
            .get_result::<PlatformSubscriptionSumRow>(conn)?
            .quantity;
        if subscription_entity_seconds > 0 {
            platform_sums.push((
                SUBSCRIPTION_METRIC.to_string(),
                subscription_entity_seconds,
            ));
        }
    }
    let platform_rates = platform_rates_for_package_tier(package_tier);
    let platform = PlatformPeriodBilling::from_entity_seconds(platform_rates, &platform_sums);

    Ok(PeriodBilling::from_metered(
        rates,
        compute_row.sum_gb_ms,
        byte_hours_active,
        byte_hours_retained,
        llm,
        platform,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use crate::services::pricing::config::{
        BASE_PROJECT_PER_HOUR, BASE_STAGE_RUNTIME_PER_HOUR, BASE_SUBSCRIPTION_MONTHLY_USD,
        BASE_UNIT_PER_HOUR, LLM_PLATFORM_MARGIN, TIER1_PLATFORM_COSTS_FACTOR,
        TIER2_PLATFORM_COSTS_FACTOR, TIER3_PLATFORM_COSTS_FACTOR,
    };
    use crate::services::pricing::llm_prices::{
        OPENAI_GPT_4O_MINI_INPUT_PER_M, OPENAI_GPT_4O_MINI_OUTPUT_PER_M,
    };
    use crate::services::pricing::platform_resources::HOURS_PER_MONTH;

    fn period_start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()
    }

    fn period_end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 2, 0, 0, 0).unwrap()
    }

    fn at(month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, month, day, hour, 0, 0).unwrap()
    }

    fn at_2024(month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, month, day, hour, 0, 0).unwrap()
    }

    fn test_id(n: u8) -> Uuid {
        Uuid::parse_str(&format!("00000000-0000-0000-0000-{n:012x}")).unwrap()
    }

    fn storage(id: u8, recorded_at: DateTime<Utc>, tier: &str, op: &str, quantity: i64) -> StorageMeterEvent {
        StorageMeterEvent {
            id: test_id(id),
            recorded_at,
            tier: tier.to_string(),
            op: op.to_string(),
            quantity,
        }
    }

    fn hot_cost(byte_hours: f64) -> f64 {
        BillingRates::test_rates().storage_active_cost(byte_hours)
    }

    fn cold_cost(byte_hours: f64) -> f64 {
        BillingRates::test_rates().storage_retained_cost(byte_hours)
    }

    fn compute(recorded_at: DateTime<Utc>, quantity: i64) -> ComputeMeterEvent {
        ComputeMeterEvent {
            recorded_at,
            metric: COMPUTE_GB_MS_METRIC.to_string(),
            quantity,
        }
    }

    fn compute_metric(recorded_at: DateTime<Utc>, metric: &str, quantity: i64) -> ComputeMeterEvent {
        ComputeMeterEvent {
            recorded_at,
            metric: metric.to_string(),
            quantity,
        }
    }

    fn llm(
        recorded_at: DateTime<Utc>,
        provider: &str,
        model: &str,
        metric: &str,
        quantity: i64,
    ) -> LlmMeterEvent {
        LlmMeterEvent {
            recorded_at,
            provider: provider.to_string(),
            model: model.to_string(),
            metric: metric.to_string(),
            quantity,
        }
    }

    fn tier_balance(events: &[StorageMeterEvent], tier: &str, before: DateTime<Utc>) -> i64 {
        events
            .iter()
            .filter(|e| e.tier == tier && e.recorded_at < before)
            .map(|e| e.signed_quantity())
            .sum()
    }

    fn assert_period(
        billing: &PeriodBilling,
        sum_gb_ms: i64,
        compute_cost: f64,
        byte_hours_active: f64,
        byte_hours_retained: f64,
        storage_hot_cost: f64,
        storage_cold_cost: f64,
        total: f64,
    ) {
        assert_eq!(billing.sum_gb_ms, sum_gb_ms, "sum_gb_ms");
        assert!((billing.compute_cost - compute_cost).abs() < 1e-9, "compute_cost");
        assert!(
            (billing.byte_hours_active - byte_hours_active).abs() < 1e-6,
            "byte_hours_active: got {} expected {}",
            billing.byte_hours_active,
            byte_hours_active
        );
        assert!(
            (billing.byte_hours_retained - byte_hours_retained).abs() < 1e-6,
            "byte_hours_retained"
        );
        assert!((billing.storage_hot_cost - storage_hot_cost).abs() < 1e-6, "storage_hot_cost");
        assert!(
            (billing.storage_cold_cost - storage_cold_cost).abs() < 1e-6,
            "storage_cold_cost"
        );
        assert_eq!(billing.sum_input_tokens, 0, "sum_input_tokens");
        assert_eq!(billing.sum_output_tokens, 0, "sum_output_tokens");
        assert!((billing.llm_input_cost).abs() < 1e-9, "llm_input_cost");
        assert!((billing.llm_output_cost).abs() < 1e-9, "llm_output_cost");
        assert!((billing.total - total).abs() < 1e-6, "total");
    }

    fn empty_platform_billing() -> PlatformPeriodBilling {
        PlatformPeriodBilling::from_entity_seconds(PlatformResourceRates {
            subscription_per_hour: 0.0,
            project_per_hour: 0.0,
            unit_per_hour: 0.0,
            stage_runtime_per_hour: 0.0,
        }, &[])
    }

    fn bill(
        compute_events: &[ComputeMeterEvent],
        storage_events: &[StorageMeterEvent],
        llm_events: &[LlmMeterEvent],
    ) -> PeriodBilling {
        let start = period_start();
        let end = period_end();
        let rates = BillingRates::test_rates();
        let sum_gb_ms = ComputeBilling::sum(compute_events, start, end);
        let byte_hours_active =
            StorageBilling::byte_hours(storage_events, STORAGE_TIER_ACTIVE, start, end);
        let byte_hours_retained =
            StorageBilling::byte_hours(storage_events, STORAGE_TIER_RETAINED, start, end);
        let sum_input_tokens =
            LlmBilling::sum(llm_events, LLM_INPUT_TOKENS_METRIC, start, end);
        let sum_output_tokens =
            LlmBilling::sum(llm_events, LLM_OUTPUT_TOKENS_METRIC, start, end);
        let llm = LlmBilling::period_cost(llm_events, start, end, 1);
        assert_eq!(llm.sum_input_tokens, sum_input_tokens);
        assert_eq!(llm.sum_output_tokens, sum_output_tokens);
        PeriodBilling::from_metered(
            rates,
            sum_gb_ms,
            byte_hours_active,
            byte_hours_retained,
            llm,
            empty_platform_billing(),
        )
    }

    #[test]
    fn t1_compute_only() {
        let billing = bill(
            &[
                compute(at(1, 1, 10), 1000),
                compute(at(1, 1, 15), 500),
                compute_metric(at(1, 1, 11), "compute.actions", 999),
            ],
            &[],
            &[],
        );
        assert_period(&billing, 1500, 1.5, 0.0, 0.0, 0.0, 0.0, 1.5);
    }

    #[test]
    fn t2_flat_hot_storage_all_day() {
        let events = [storage(
            1,
            at_2024(12, 31, 0),
            STORAGE_TIER_ACTIVE,
            "incr",
            1_000_000,
        )];
        let start = period_start();
        assert_eq!(
            tier_balance(&events, STORAGE_TIER_ACTIVE, start),
            1_000_000,
            "opening_hot"
        );
        let billing = bill(&[], &events, &[]);
        let byte_hours = 1_000_000.0 * 24.0;
        assert_period(
            &billing,
            0,
            0.0,
            byte_hours,
            0.0,
            hot_cost(byte_hours),
            0.0,
            hot_cost(byte_hours),
        );
    }

    #[test]
    fn t3_incr_mid_period() {
        let billing = bill(
            &[],
            &[storage(1, at(1, 1, 12), STORAGE_TIER_ACTIVE, "incr", 2_000_000)],
            &[],
        );
        let byte_hours = 2_000_000.0 * 12.0;
        assert_period(
            &billing,
            0,
            0.0,
            byte_hours,
            0.0,
            hot_cost(byte_hours),
            0.0,
            hot_cost(byte_hours),
        );
    }

    #[test]
    fn t4_decr_mid_period() {
        let billing = bill(
            &[],
            &[
                storage(1, at_2024(12, 31, 0), STORAGE_TIER_ACTIVE, "incr", 3_000_000),
                storage(2, at(1, 1, 12), STORAGE_TIER_ACTIVE, "decr", 3_000_000),
            ],
            &[],
        );
        let byte_hours = 3_000_000.0 * 12.0;
        assert_period(
            &billing,
            0,
            0.0,
            byte_hours,
            0.0,
            hot_cost(byte_hours),
            0.0,
            hot_cost(byte_hours),
        );
    }

    #[test]
    fn t5_tier_move_workflow_terminal() {
        let events = [
            storage(1, at_2024(12, 31, 0), STORAGE_TIER_ACTIVE, "incr", 5_000),
            storage(2, at(1, 1, 12), STORAGE_TIER_ACTIVE, "decr", 5_000),
            storage(3, at(1, 1, 12), STORAGE_TIER_RETAINED, "incr", 5_000),
        ];
        let billing = bill(&[], &events, &[]);
        let bh = 60_000.0;
        assert_period(
            &billing,
            0,
            0.0,
            bh,
            bh,
            hot_cost(bh),
            cold_cost(bh),
            hot_cost(bh) + cold_cost(bh),
        );
        let end = period_end();
        assert_eq!(
            tier_balance(&events, STORAGE_TIER_ACTIVE, end),
            0,
            "hot balance after tier move"
        );
        assert_eq!(
            tier_balance(&events, STORAGE_TIER_RETAINED, end),
            5_000,
            "cold balance after tier move"
        );
    }

    #[test]
    fn t6_opening_balance_from_pre_period_events() {
        let events = [
            storage(1, at_2024(12, 31, 23), STORAGE_TIER_ACTIVE, "incr", 1_000_000),
            storage(2, at(1, 1, 6), STORAGE_TIER_ACTIVE, "incr", 500_000),
        ];
        assert_eq!(
            tier_balance(&events, STORAGE_TIER_ACTIVE, period_start()),
            1_000_000,
            "opening at period start"
        );
        let billing = bill(&[], &events, &[]);
        let byte_hours = 1_000_000.0 * 6.0 + 1_500_000.0 * 18.0;
        assert_eq!(byte_hours, 33_000_000.0, "byte_hours_active");
        assert_period(
            &billing,
            0,
            0.0,
            byte_hours,
            0.0,
            hot_cost(byte_hours),
            0.0,
            hot_cost(byte_hours),
        );
    }

    #[test]
    fn t7_negative_balance_clamp() {
        let events = [storage(1, at(1, 1, 10), STORAGE_TIER_ACTIVE, "decr", 1000)];
        assert_eq!(
            tier_balance(&events, STORAGE_TIER_ACTIVE, period_start()),
            0,
            "opening"
        );
        let billing = bill(&[], &events, &[]);
        assert_eq!(billing.byte_hours_active, 0.0, "byte_hours_active");
        assert_period(&billing, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    }

    #[test]
    fn t8_empty_period() {
        let billing = bill(&[], &[], &[]);
        assert_period(&billing, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    }

    #[test]
    fn t9_mixed_compute_hot_cold() {
        let billing = bill(
            &[
                compute(at(1, 1, 10), 1000),
                compute(at(1, 1, 15), 500),
            ],
            &[
                storage(1, at_2024(12, 31, 0), STORAGE_TIER_ACTIVE, "incr", 5_000),
                storage(2, at(1, 1, 12), STORAGE_TIER_ACTIVE, "decr", 5_000),
                storage(3, at(1, 1, 12), STORAGE_TIER_RETAINED, "incr", 5_000),
            ],
            &[],
        );
        let bh = 60_000.0;
        assert_period(
            &billing,
            1500,
            1.5,
            bh,
            bh,
            hot_cost(bh),
            cold_cost(bh),
            1.5 + hot_cost(bh) + cold_cost(bh),
        );
    }

    #[test]
    fn t10_same_timestamp_ordering() {
        let start = period_start();
        let end = period_end();
        let rates = BillingRates::test_rates();
        let opening = storage(1, at_2024(12, 31, 0), STORAGE_TIER_ACTIVE, "incr", 5_000);

        let decr_active_first = [
            opening.clone(),
            storage(2, at(1, 1, 12), STORAGE_TIER_ACTIVE, "decr", 5_000),
            storage(3, at(1, 1, 12), STORAGE_TIER_RETAINED, "incr", 5_000),
        ];
        let incr_retained_first = [
            opening,
            storage(2, at(1, 1, 12), STORAGE_TIER_RETAINED, "incr", 5_000),
            storage(3, at(1, 1, 12), STORAGE_TIER_ACTIVE, "decr", 5_000),
        ];

        let bill_storage = |events: &[StorageMeterEvent]| {
            PeriodBilling::from_metered(
                rates,
                0,
                StorageBilling::byte_hours(events, STORAGE_TIER_ACTIVE, start, end),
                StorageBilling::byte_hours(events, STORAGE_TIER_RETAINED, start, end),
                LlmPeriodCost {
                    sum_input_tokens: 0,
                    sum_output_tokens: 0,
                    input_cost: 0.0,
                    output_cost: 0.0,
                },
                empty_platform_billing(),
            )
        };

        let decr_first = bill_storage(&decr_active_first);
        let incr_first = bill_storage(&incr_retained_first);
        let bh = 60_000.0;
        assert_period(
            &decr_first,
            0,
            0.0,
            bh,
            bh,
            hot_cost(bh),
            cold_cost(bh),
            hot_cost(bh) + cold_cost(bh),
        );
        assert_eq!(decr_first, incr_first, "same-timestamp row order must not change totals");
    }

    #[test]
    fn t11_llm_tokens_only() {
        let input_tokens = 1_000_000_i64;
        let output_tokens = 500_000_i64;
        let billing = bill(
            &[],
            &[],
            &[
                llm(
                    at(1, 1, 10),
                    "openai",
                    "gpt-4o-mini",
                    LLM_INPUT_TOKENS_METRIC,
                    input_tokens,
                ),
                llm(
                    at(1, 1, 11),
                    "openai",
                    "gpt-4o-mini",
                    LLM_OUTPUT_TOKENS_METRIC,
                    output_tokens,
                ),
                llm(at(1, 1, 12), "openai", "gpt-4o-mini", "other.metric", 9_999),
            ],
        );
        let margin = 1.0 + LLM_PLATFORM_MARGIN;
        let expected_input =
            input_tokens as f64 * OPENAI_GPT_4O_MINI_INPUT_PER_M / 1_000_000.0 * margin;
        let expected_output =
            output_tokens as f64 * OPENAI_GPT_4O_MINI_OUTPUT_PER_M / 1_000_000.0 * margin;
        assert_eq!(billing.sum_input_tokens, input_tokens);
        assert_eq!(billing.sum_output_tokens, output_tokens);
        assert_eq!(billing.llm_input_cost, expected_input);
        assert_eq!(billing.llm_output_cost, expected_output);
        assert_eq!(billing.total, expected_input + expected_output);
    }

    #[test]
    fn validate_billing_period_rejects_over_two_months() {
        let start = period_start();
        let end = start + chrono::Duration::days(63);
        assert!(validate_billing_period(start, end).is_err());
    }

    fn test_platform_rates() -> PlatformResourceRates {
        PlatformResourceRates {
            subscription_per_hour: 1.0,
            project_per_hour: 0.01,
            unit_per_hour: 0.001,
            stage_runtime_per_hour: 0.1,
        }
    }

    fn platform_from_sums(sums: &[(String, i64)]) -> PlatformPeriodBilling {
        PlatformPeriodBilling::from_entity_seconds(test_platform_rates(), sums)
    }

    fn assert_platform(
        billing: &PlatformPeriodBilling,
        subscription_entity_seconds: i64,
        project_entity_seconds: i64,
        unit_entity_seconds: i64,
        stage_runtime_entity_seconds: i64,
        subscription_cost: f64,
        project_cost: f64,
        unit_cost: f64,
        stage_runtime_cost: f64,
    ) {
        assert_eq!(
            billing.subscription_entity_seconds, subscription_entity_seconds,
            "subscription_entity_seconds"
        );
        assert_eq!(
            billing.project_entity_seconds, project_entity_seconds,
            "project_entity_seconds"
        );
        assert_eq!(billing.unit_entity_seconds, unit_entity_seconds, "unit_entity_seconds");
        assert_eq!(
            billing.stage_runtime_entity_seconds, stage_runtime_entity_seconds,
            "stage_runtime_entity_seconds"
        );
        assert!(
            (billing.subscription_cost - subscription_cost).abs() < 1e-12,
            "subscription_cost: got {} expected {}",
            billing.subscription_cost,
            subscription_cost
        );
        assert!(
            (billing.project_cost - project_cost).abs() < 1e-12,
            "project_cost: got {} expected {}",
            billing.project_cost,
            project_cost
        );
        assert!(
            (billing.unit_cost - unit_cost).abs() < 1e-12,
            "unit_cost: got {} expected {}",
            billing.unit_cost,
            unit_cost
        );
        assert!(
            (billing.stage_runtime_cost - stage_runtime_cost).abs() < 1e-12,
            "stage_runtime_cost: got {} expected {}",
            billing.stage_runtime_cost,
            stage_runtime_cost
        );
        assert!(
            (billing.total_cost()
                - (subscription_cost + project_cost + unit_cost + stage_runtime_cost))
                .abs()
                < 1e-12,
            "total_cost"
        );
    }

    #[test]
    fn platform_empty_sums_are_zero() {
        let billing = platform_from_sums(&[]);
        assert_platform(&billing, 0, 0, 0, 0, 0.0, 0.0, 0.0, 0.0);
    }

    #[test]
    fn platform_project_one_entity_hour() {
        let billing = platform_from_sums(&[(PROJECT_METRIC.to_string(), 3_600)]);
        assert_platform(&billing, 0, 3_600, 0, 0, 0.0, 0.01, 0.0, 0.0);
    }

    #[test]
    fn platform_subscription_one_entity_hour() {
        let billing = platform_from_sums(&[(SUBSCRIPTION_METRIC.to_string(), 3_600)]);
        assert_platform(&billing, 3_600, 0, 0, 0, 1.0, 0.0, 0.0, 0.0);
    }

    #[test]
    fn platform_units_scaled_by_count_and_duration() {
        let entity_seconds = 5_i64 * 1_800;
        let billing = platform_from_sums(&[(UNIT_METRIC.to_string(), entity_seconds)]);
        let expected = entity_seconds as f64 * (0.001 / 3600.0);
        assert_platform(&billing, 0, 0, entity_seconds, 0, 0.0, 0.0, expected, 0.0);
    }

    #[test]
    fn platform_stage_runtime_metering() {
        let entity_seconds = 2_i64 * 3_600;
        let billing = platform_from_sums(&[(STAGE_RUNTIME_METRIC.to_string(), entity_seconds)]);
        let expected = entity_seconds as f64 * (0.1 / 3600.0);
        assert_platform(&billing, 0, 0, 0, entity_seconds, 0.0, 0.0, 0.0, expected);
    }

    #[test]
    fn platform_sums_multiple_rows_per_metric() {
        let billing = platform_from_sums(&[
            (PROJECT_METRIC.to_string(), 1_800),
            (PROJECT_METRIC.to_string(), 1_800),
            (UNIT_METRIC.to_string(), 600),
            (UNIT_METRIC.to_string(), 400),
        ]);
        let project_cost = 3_600.0 * (0.01 / 3600.0);
        let unit_cost = 1_000.0 * (0.001 / 3600.0);
        assert_platform(&billing, 0, 3_600, 1_000, 0, 0.0, project_cost, unit_cost, 0.0);
    }

    #[test]
    fn platform_all_metrics_combined() {
        let billing = platform_from_sums(&[
            (SUBSCRIPTION_METRIC.to_string(), 3_600),
            (PROJECT_METRIC.to_string(), 3_600),
            (UNIT_METRIC.to_string(), 3_600),
            (STAGE_RUNTIME_METRIC.to_string(), 3_600),
        ]);
        assert_platform(&billing, 3_600, 3_600, 3_600, 3_600, 1.0, 0.01, 0.001, 0.1);
    }

    #[test]
    fn platform_ignores_unknown_metric() {
        let billing = platform_from_sums(&[
            (PROJECT_METRIC.to_string(), 3_600),
            ("unknown.metric".to_string(), 9_999),
        ]);
        assert_platform(&billing, 0, 3_600, 0, 0, 0.0, 0.01, 0.0, 0.0);
    }

    #[test]
    fn platform_zero_and_negative_entity_seconds_are_free() {
        let rates = test_platform_rates();
        assert_eq!(rates.cost_for_entity_seconds(PROJECT_METRIC, 0), 0.0);
        assert_eq!(rates.cost_for_entity_seconds(PROJECT_METRIC, -100), 0.0);
        assert_eq!(rates.cost_for_entity_seconds("unknown", 3_600), 0.0);

        let billing = platform_from_sums(&[(PROJECT_METRIC.to_string(), 0)]);
        assert_platform(&billing, 0, 0, 0, 0, 0.0, 0.0, 0.0, 0.0);
    }

    #[test]
    fn platform_entity_second_rates_match_hourly_rates() {
        let rates = test_platform_rates();
        assert!((rates.subscription_per_entity_second() - 1.0 / 3600.0).abs() < 1e-15);
        assert!((rates.project_per_entity_second() - 0.01 / 3600.0).abs() < 1e-15);
        assert!((rates.unit_per_entity_second() - 0.001 / 3600.0).abs() < 1e-15);
        assert!((rates.stage_runtime_per_entity_second() - 0.1 / 3600.0).abs() < 1e-15);
    }

    #[test]
    fn platform_rates_for_package_tier_tier1() {
        let rates = platform_rates_for_package_tier(1);
        assert_eq!(
            rates.subscription_per_hour,
            BASE_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH * TIER1_PLATFORM_COSTS_FACTOR
        );
        assert_eq!(rates.project_per_hour, BASE_PROJECT_PER_HOUR * TIER1_PLATFORM_COSTS_FACTOR);
        assert_eq!(rates.unit_per_hour, BASE_UNIT_PER_HOUR * TIER1_PLATFORM_COSTS_FACTOR);
        assert_eq!(
            rates.stage_runtime_per_hour,
            BASE_STAGE_RUNTIME_PER_HOUR * TIER1_PLATFORM_COSTS_FACTOR
        );
    }

    #[test]
    fn platform_rates_for_package_tier_tier2() {
        let rates = platform_rates_for_package_tier(2);
        assert_eq!(
            rates.subscription_per_hour,
            BASE_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH * TIER2_PLATFORM_COSTS_FACTOR
        );
        assert_eq!(rates.project_per_hour, BASE_PROJECT_PER_HOUR * TIER2_PLATFORM_COSTS_FACTOR);
        assert_eq!(rates.unit_per_hour, BASE_UNIT_PER_HOUR * TIER2_PLATFORM_COSTS_FACTOR);
        assert_eq!(
            rates.stage_runtime_per_hour,
            BASE_STAGE_RUNTIME_PER_HOUR * TIER2_PLATFORM_COSTS_FACTOR
        );
    }

    #[test]
    fn platform_rates_for_package_tier_tier3_plus() {
        let rates = platform_rates_for_package_tier(3);
        assert_eq!(
            rates.subscription_per_hour,
            BASE_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH * TIER3_PLATFORM_COSTS_FACTOR
        );
        assert_eq!(rates.project_per_hour, BASE_PROJECT_PER_HOUR * TIER3_PLATFORM_COSTS_FACTOR);
        assert_eq!(rates.unit_per_hour, BASE_UNIT_PER_HOUR * TIER3_PLATFORM_COSTS_FACTOR);
        assert_eq!(
            rates.stage_runtime_per_hour,
            BASE_STAGE_RUNTIME_PER_HOUR * TIER3_PLATFORM_COSTS_FACTOR
        );

        assert_eq!(
            platform_rates_for_package_tier(5),
            platform_rates_for_package_tier(4)
        );
    }

    #[test]
    fn platform_rates_non_positive_tier_defaults_to_tier1() {
        assert_eq!(
            platform_rates_for_package_tier(0),
            platform_rates_for_package_tier(1)
        );
        assert_eq!(
            platform_rates_for_package_tier(-1),
            platform_rates_for_package_tier(1)
        );
    }

    #[test]
    fn platform_tier2_subscription_cost_from_entity_seconds() {
        let rates = platform_rates_for_package_tier(2);
        let entity_seconds = 3_600;
        let billing = PlatformPeriodBilling::from_entity_seconds(
            rates,
            &[(SUBSCRIPTION_METRIC.to_string(), entity_seconds)],
        );
        assert_eq!(
            billing.subscription_cost,
            rates.cost_for_entity_seconds(SUBSCRIPTION_METRIC, entity_seconds)
        );
        assert_eq!(billing.subscription_entity_seconds, entity_seconds);
    }

    #[test]
    fn period_billing_includes_platform_in_total() {
        let rates = BillingRates::test_rates();
        let platform = platform_from_sums(&[
            (PROJECT_METRIC.to_string(), 3_600),
            (UNIT_METRIC.to_string(), 3_600),
        ]);
        let llm = LlmPeriodCost {
            sum_input_tokens: 0,
            sum_output_tokens: 0,
            input_cost: 0.0,
            output_cost: 0.0,
        };
        let billing = PeriodBilling::from_metered(rates, 0, 0.0, 0.0, llm, platform);
        let platform_total = 0.01 + 0.001;
        assert!((billing.platform.total_cost() - platform_total).abs() < 1e-12);
        assert!((billing.total - platform_total).abs() < 1e-12);
    }

    #[test]
    fn t12_platform_only_project_meter() {
        let platform = platform_from_sums(&[(PROJECT_METRIC.to_string(), 3_600)]);
        let billing = PeriodBilling::from_metered(
            BillingRates::test_rates(),
            0,
            0.0,
            0.0,
            LlmPeriodCost {
                sum_input_tokens: 0,
                sum_output_tokens: 0,
                input_cost: 0.0,
                output_cost: 0.0,
            },
            platform,
        );
        assert_platform(&billing.platform, 0, 3_600, 0, 0, 0.0, 0.01, 0.0, 0.0);
        assert!((billing.total - 0.01).abs() < 1e-12);
    }

    #[test]
    fn t13_mixed_compute_storage_llm_and_platform() {
        let compute_billing = bill(
            &[compute(at(1, 1, 10), 1000)],
            &[storage(
                1,
                at_2024(12, 31, 0),
                STORAGE_TIER_ACTIVE,
                "incr",
                1_000_000,
            )],
            &[llm(
                at(1, 1, 10),
                "openai",
                "gpt-4o-mini",
                LLM_INPUT_TOKENS_METRIC,
                1_000_000,
            )],
        );
        let platform = platform_from_sums(&[
            (SUBSCRIPTION_METRIC.to_string(), 3_600),
            (PROJECT_METRIC.to_string(), 3_600),
            (UNIT_METRIC.to_string(), 7_200),
            (STAGE_RUNTIME_METRIC.to_string(), 3_600),
        ]);
        let billing = PeriodBilling::from_metered(
            BillingRates::test_rates(),
            compute_billing.sum_gb_ms,
            compute_billing.byte_hours_active,
            compute_billing.byte_hours_retained,
            LlmPeriodCost {
                sum_input_tokens: compute_billing.sum_input_tokens,
                sum_output_tokens: compute_billing.sum_output_tokens,
                input_cost: compute_billing.llm_input_cost,
                output_cost: compute_billing.llm_output_cost,
            },
            platform,
        );

        let compute_rates = BillingRates::test_rates();
        let platform_rates = test_platform_rates();
        let byte_hours = 1_000_000.0 * 24.0;
        let compute_cost = 1000.0 * compute_rates.compute_gb_ms;
        let storage_cost = compute_rates.storage_active_cost(byte_hours);
        let llm_input_cost = OPENAI_GPT_4O_MINI_INPUT_PER_M * (1.0 + LLM_PLATFORM_MARGIN);
        let subscription_cost = platform_rates.subscription_per_hour;
        let project_cost = platform_rates.project_per_hour;
        let unit_cost = 2.0 * platform_rates.unit_per_hour;
        let stage_runtime_cost = platform_rates.stage_runtime_per_hour;
        let platform_total = subscription_cost + project_cost + unit_cost + stage_runtime_cost;
        let expected_total = compute_cost + storage_cost + llm_input_cost + platform_total;

        assert_eq!(billing.sum_gb_ms, 1000);
        assert_eq!(billing.compute_cost, compute_cost);
        assert_eq!(billing.byte_hours_active, byte_hours);
        assert_eq!(billing.storage_hot_cost, storage_cost);
        assert_eq!(billing.sum_input_tokens, 1_000_000);
        assert_eq!(billing.llm_input_cost, llm_input_cost);
        assert_platform(
            &billing.platform,
            3_600,
            3_600,
            7_200,
            3_600,
            subscription_cost,
            project_cost,
            unit_cost,
            stage_runtime_cost,
        );
        assert_eq!(billing.total, expected_total);
    }

    #[test]
    fn t14_platform_partial_hour_fractional_cost() {
        let entity_seconds = 1_800;
        let billing = platform_from_sums(&[(PROJECT_METRIC.to_string(), entity_seconds)]);
        let expected = 0.5 * 0.01;
        assert_platform(&billing, 0, entity_seconds, 0, 0, 0.0, expected, 0.0, 0.0);
    }
}
