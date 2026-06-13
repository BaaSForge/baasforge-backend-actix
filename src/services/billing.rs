use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Text, Timestamptz, Uuid as SqlUuid};
use diesel::QueryableByName;
use uuid::Uuid;

pub const MAX_BILLING_PERIOD_DAYS: i64 = 62;

pub const COMPUTE_GB_MS_METRIC: &str = "compute.gb_ms";
pub const STORAGE_TIER_ACTIVE: &str = "active";
pub const STORAGE_TIER_RETAINED: &str = "retained";

// TODO: daily balance snapshots / project_usage_daily_totals for faster billing

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BillingRates {
    pub compute_gb_ms: f64,
    pub storage_active: f64,
    pub storage_retained: f64,
}

impl BillingRates {
    pub fn test_rates() -> Self {
        Self {
            compute_gb_ms: 0.001,
            storage_active: 0.01,
            storage_retained: 0.001,
        }
    }
}

fn effective_tier(tier: i32) -> i32 {
    if tier <= 0 {
        1
    } else {
        tier
    }
}

pub fn billing_rates_for_package_tier(tier: i32) -> BillingRates {
    match effective_tier(tier) {
        1 => BillingRates {
            compute_gb_ms: 1.0e-9,
            storage_active: 3.0e-12,
            storage_retained: 3.0e-13,
        },
        2 => BillingRates {
            compute_gb_ms: 8.0e-10,
            storage_active: 2.4e-12,
            storage_retained: 2.4e-13,
        },
        _ => BillingRates {
            compute_gb_ms: 6.0e-10,
            storage_active: 1.8e-12,
            storage_retained: 1.8e-13,
        },
    }
}

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
pub struct PeriodBilling {
    pub sum_gb_ms: i64,
    pub compute_cost: f64,
    pub byte_hours_active: f64,
    pub byte_hours_retained: f64,
    pub storage_hot_cost: f64,
    pub storage_cold_cost: f64,
    pub total: f64,
}

impl PeriodBilling {
    pub fn from_metered(
        rates: BillingRates,
        sum_gb_ms: i64,
        byte_hours_active: f64,
        byte_hours_retained: f64,
    ) -> Self {
        let compute_cost = sum_gb_ms as f64 * rates.compute_gb_ms;
        let storage_hot_cost = byte_hours_active * rates.storage_active;
        let storage_cold_cost = byte_hours_retained * rates.storage_retained;
        Self {
            sum_gb_ms,
            compute_cost,
            byte_hours_active,
            byte_hours_retained,
            storage_hot_cost,
            storage_cold_cost,
            total: compute_cost + storage_hot_cost + storage_cold_cost,
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

#[derive(QueryableByName)]
struct ComputeSumRow {
    #[diesel(sql_type = BigInt)]
    sum_gb_ms: i64,
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

    Ok(PeriodBilling::from_metered(
        rates,
        compute_row.sum_gb_ms,
        byte_hours_active,
        byte_hours_retained,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
        assert!((billing.total - total).abs() < 1e-6, "total");
    }

    fn bill(
        compute_events: &[ComputeMeterEvent],
        storage_events: &[StorageMeterEvent],
    ) -> PeriodBilling {
        let start = period_start();
        let end = period_end();
        let rates = BillingRates::test_rates();
        let sum_gb_ms = ComputeBilling::sum(compute_events, start, end);
        let byte_hours_active =
            StorageBilling::byte_hours(storage_events, STORAGE_TIER_ACTIVE, start, end);
        let byte_hours_retained =
            StorageBilling::byte_hours(storage_events, STORAGE_TIER_RETAINED, start, end);
        PeriodBilling::from_metered(rates, sum_gb_ms, byte_hours_active, byte_hours_retained)
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
        let billing = bill(&[], &events);
        let byte_hours = 1_000_000.0 * 24.0;
        assert_period(
            &billing,
            0,
            0.0,
            byte_hours,
            0.0,
            byte_hours * 0.01,
            0.0,
            byte_hours * 0.01,
        );
    }

    #[test]
    fn t3_incr_mid_period() {
        let billing = bill(
            &[],
            &[storage(1, at(1, 1, 12), STORAGE_TIER_ACTIVE, "incr", 2_000_000)],
        );
        let byte_hours = 2_000_000.0 * 12.0;
        assert_period(
            &billing,
            0,
            0.0,
            byte_hours,
            0.0,
            byte_hours * 0.01,
            0.0,
            byte_hours * 0.01,
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
        );
        let byte_hours = 3_000_000.0 * 12.0;
        assert_period(
            &billing,
            0,
            0.0,
            byte_hours,
            0.0,
            byte_hours * 0.01,
            0.0,
            byte_hours * 0.01,
        );
    }

    #[test]
    fn t5_tier_move_workflow_terminal() {
        let events = [
            storage(1, at_2024(12, 31, 0), STORAGE_TIER_ACTIVE, "incr", 5_000),
            storage(2, at(1, 1, 12), STORAGE_TIER_ACTIVE, "decr", 5_000),
            storage(3, at(1, 1, 12), STORAGE_TIER_RETAINED, "incr", 5_000),
        ];
        let billing = bill(&[], &events);
        assert_period(&billing, 0, 0.0, 60_000.0, 60_000.0, 600.0, 60.0, 660.0);
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
        let billing = bill(&[], &events);
        let byte_hours = 1_000_000.0 * 6.0 + 1_500_000.0 * 18.0;
        assert_eq!(byte_hours, 33_000_000.0, "byte_hours_active");
        assert_period(
            &billing,
            0,
            0.0,
            byte_hours,
            0.0,
            byte_hours * 0.01,
            0.0,
            byte_hours * 0.01,
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
        let billing = bill(&[], &events);
        assert_eq!(billing.byte_hours_active, 0.0, "byte_hours_active");
        assert_period(&billing, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    }

    #[test]
    fn t8_empty_period() {
        let billing = bill(&[], &[]);
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
        );
        assert_period(&billing, 1500, 1.5, 60_000.0, 60_000.0, 600.0, 60.0, 661.5);
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
            )
        };

        let decr_first = bill_storage(&decr_active_first);
        let incr_first = bill_storage(&incr_retained_first);
        assert_period(&decr_first, 0, 0.0, 60_000.0, 60_000.0, 600.0, 60.0, 660.0);
        assert_eq!(decr_first, incr_first, "same-timestamp row order must not change totals");
    }

    #[test]
    fn validate_billing_period_rejects_over_two_months() {
        let start = period_start();
        let end = start + chrono::Duration::days(63);
        assert!(validate_billing_period(start, end).is_err());
    }
}
