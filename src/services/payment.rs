use crate::models::packages::Package;
use crate::models::payments::{BillingPeriod, PaymentInsert, PaymentResponse};
use crate::schema::{packages, payments};
use crate::services::balance::unfreeze_user_billing;
use crate::services::pricing::package_subscription_amount;
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug)]
pub struct GrantedPackagePayment {
    pub package: Package,
    pub amount: f64,
    pub expires_at: DateTime<Utc>,
    pub billing_period: BillingPeriod,
}

pub fn grant_package_payment<C>(
    conn: &mut C,
    user_id: Uuid,
    package: &Package,
    billing_period: BillingPeriod,
    gateway_metadata: Option<Value>,
) -> Result<GrantedPackagePayment, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let (paid_packages, _) = get_user_active_paid_packages(conn, user_id)?;
    let now = Utc::now();
    let is_annual = billing_period == BillingPeriod::Annual;
    // Charged via Stripe and credited to balance — always the discounted sale price.
    let amount = package_subscription_amount(package.price, package.discount_percent, is_annual);
    let expires_at = if is_annual {
        now + Duration::days(365)
    } else {
        now + Duration::days(30)
    };
    if !paid_packages.is_empty() {
        let not_expired = payments::expires_at
            .is_null()
            .or(payments::expires_at.gt(now));
        diesel::update(
            payments::table
                .filter(payments::user_id.eq(user_id))
                .filter(payments::is_success.eq(true))
                .filter(payments::cancelled_at.is_null())
                .filter(not_expired),
        )
        .set(payments::expires_at.eq(now))
        .execute(conn)?;
    }
    diesel::insert_into(payments::table)
        .values(&PaymentInsert {
            user_id,
            amount,
            package_id: package.id,
            package_title: Some(package.title.clone()),
            package_slug: Some(package.slug.clone()),
            package_tier: package.tier,
            package_price: Some(package.price),
            package_discount: package.discount_percent,
            package_is_tiral: package.is_trial.unwrap_or(false),
            is_success: true,
            gateway_metadata,
            expires_at: Some(expires_at),
            billing_period,
        })
        .execute(conn)?;
    unfreeze_user_billing(conn, user_id)?;
    Ok(GrantedPackagePayment {
        package: package.clone(),
        amount,
        expires_at,
        billing_period,
    })
}

pub fn get_user_active_paid_packages<C>(
    conn: &mut C,
    user_id: Uuid,
) -> Result<(Vec<(PaymentResponse, Package)>, i32), diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let now = Utc::now();
    let not_expired = payments::expires_at
        .is_null()
        .or(payments::expires_at.gt(now));

    let payments_with_packages: Vec<(PaymentResponse, Package)> = payments::table
        .inner_join(packages::table.on(payments::package_id.eq(packages::id)))
        .filter(payments::user_id.eq(user_id))
        .filter(payments::is_success.eq(true))
        .filter(payments::cancelled_at.is_null())
        .filter(not_expired)
        .order(payments::id.desc())
        .select((PaymentResponse::as_select(), Package::as_select()))
        .load(conn)?;

    let largest_tier: i32 = payments::table
        .filter(payments::user_id.eq(user_id))
        .filter(payments::is_success.eq(true))
        .filter(payments::cancelled_at.is_null())
        .filter(
            payments::expires_at
                .is_null()
                .or(payments::expires_at.gt(now)),
        )
        .select(diesel::dsl::max(payments::package_tier))
        .get_result::<Option<i32>>(conn)?
        .unwrap_or(0);

    Ok((payments_with_packages, largest_tier))
}

#[derive(Debug, Clone, Copy, Queryable, Selectable)]
#[diesel(table_name = payments)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct PaymentTierCoverage {
    package_tier: i32,
    created_at: DateTime<Utc>,
    cancelled_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

impl PaymentTierCoverage {
    fn coverage_end(&self) -> Option<DateTime<Utc>> {
        match (self.cancelled_at, self.expires_at) {
            (Some(c), Some(e)) => Some(c.min(e)),
            (Some(c), None) => Some(c),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        }
    }

    fn covers(&self, at: DateTime<Utc>) -> bool {
        self.created_at <= at && self.coverage_end().is_none_or(|end| at < end)
    }
}

fn load_overlapping_tier_coverages<C>(
    conn: &mut C,
    user_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<PaymentTierCoverage>, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let not_ended_before_from = payments::cancelled_at
        .is_null()
        .or(payments::cancelled_at.gt(from))
        .and(
            payments::expires_at
                .is_null()
                .or(payments::expires_at.gt(from)),
        );

    payments::table
        .filter(payments::user_id.eq(user_id))
        .filter(payments::is_success.eq(true))
        .filter(payments::created_at.lt(to))
        .filter(not_ended_before_from)
        .select(PaymentTierCoverage::as_select())
        .load(conn)
}

fn tier_at_coverages(coverages: &[PaymentTierCoverage], at: DateTime<Utc>) -> i32 {
    coverages
        .iter()
        .filter(|c| c.covers(at))
        .map(|c| c.package_tier)
        .max()
        .filter(|&tier| tier > 0)
        .unwrap_or(1)
}

fn segments_from_coverages(
    coverages: &[PaymentTierCoverage],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<PackageTierSegment> {
    if from >= to {
        return Vec::new();
    }

    let mut points = vec![from, to];
    for coverage in coverages {
        if coverage.created_at > from && coverage.created_at < to {
            points.push(coverage.created_at);
        }
        if let Some(end) = coverage.coverage_end()
            && end > from
            && end < to
        {
            points.push(end);
        }
    }
    points.sort_unstable();
    points.dedup();

    let mut segments = Vec::new();
    for window in points.windows(2) {
        let seg_start = window[0];
        let seg_end = window[1];
        if seg_start >= seg_end {
            continue;
        }
        let tier = tier_at_coverages(coverages, seg_start);
        match segments.last_mut() {
            Some((_, end, prev_tier)) if *prev_tier == tier => *end = seg_end,
            _ => segments.push((seg_start, seg_end, tier)),
        }
    }
    segments
}

pub fn package_tier_at<C>(
    conn: &mut C,
    user_id: Uuid,
    at: DateTime<Utc>,
) -> Result<i32, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let coverages = load_overlapping_tier_coverages(
        conn,
        user_id,
        at,
        at + chrono::Duration::microseconds(1),
    )?;
    Ok(tier_at_coverages(&coverages, at))
}

pub type PackageTierSegment = (DateTime<Utc>, DateTime<Utc>, i32);

pub fn package_tier_segments<C>(
    conn: &mut C,
    user_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<PackageTierSegment>, diesel::result::Error>
where
    C: diesel::connection::Connection<Backend = diesel::pg::Pg>
        + diesel::connection::LoadConnection,
{
    let coverages = load_overlapping_tier_coverages(conn, user_id, from, to)?;
    Ok(segments_from_coverages(&coverages, from, to))
}

pub const PACKAGE_PAID_FEATURES: [(&str, &[PaidFeature]); 4] = [
    ("free-trial", &[]),
    ("basic", &[PaidFeature::LotsOfProjects]),
    (
        "pro",
        &[
            PaidFeature::LotsOfProjects,
            PaidFeature::MonitoringDashboard,
        ],
    ),
    (
        "ultra",
        &[
            PaidFeature::LotsOfProjects,
            PaidFeature::MonitoringDashboard,
            PaidFeature::TeamCollaboration,
        ],
    ),
];

fn package_order() -> Vec<&'static str> {
    PACKAGE_PAID_FEATURES.iter().map(|(k, _)| *k).collect()
}

#[derive(Debug, Hash, Eq, PartialEq, Clone, Serialize)]
pub enum PaidFeature {
    LotsOfProjects,
    MonitoringDashboard,
    TeamCollaboration,
}

#[derive(Debug, Clone, Serialize)]
pub struct MissingFeature {
    pub feature: PaidFeature,
    pub needed_package_slug: Option<&'static str>,
}

fn missing_features(user_accesses: &[PaidFeature], required: &[PaidFeature]) -> Vec<PaidFeature>
where
    PaidFeature: Clone + PartialEq,
{
    required
        .iter()
        .filter(|f| !user_accesses.contains(f))
        .cloned()
        .collect()
}

fn least_package_needed_for_feature(feature: &PaidFeature) -> Option<&'static str> {
    PACKAGE_PAID_FEATURES.iter().find_map(|(pkg, features)| {
        if features.contains(feature) {
            Some(*pkg)
        } else {
            None
        }
    })
}

fn features_from_payments(payments: &[PaymentResponse]) -> Vec<PaidFeature> {
    payments
        .iter()
        .flat_map(|p| {
            PACKAGE_PAID_FEATURES
                .iter()
                .find(|(pkg, _)| Some(*pkg) == p.package_slug.as_deref())
                .into_iter()
                .flat_map(|(_, features)| features.iter().cloned())
        })
        .collect()
}

pub fn user_missing_paid_features(
    active_payments: &[PaymentResponse],
    required_features: &[PaidFeature],
) -> Vec<MissingFeature>
where
    PaidFeature: Clone + PartialEq,
{
    let user_features = features_from_payments(active_payments);
    let missing = missing_features(&user_features, required_features);

    missing
        .into_iter()
        .map(|f| MissingFeature {
            needed_package_slug: least_package_needed_for_feature(&f),
            feature: f,
        })
        .collect()
}

pub fn most_complete_needed_package(missing: &[MissingFeature]) -> Option<&'static str> {
    let package_order = package_order();
    let package_rank = |slug: &str| package_order.iter().position(|s| s == &slug);

    missing
        .iter()
        .filter_map(|f| f.needed_package_slug)
        .filter_map(|slug| package_rank(slug).map(|rank| (rank, slug)))
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, slug)| slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, day, hour, 0, 0).unwrap()
    }

    fn coverage(
        tier: i32,
        created_at: DateTime<Utc>,
        cancelled_at: Option<DateTime<Utc>>,
        expires_at: Option<DateTime<Utc>>,
    ) -> PaymentTierCoverage {
        PaymentTierCoverage {
            package_tier: tier,
            created_at,
            cancelled_at,
            expires_at,
        }
    }

    #[test]
    fn no_package_defaults_to_tier_1() {
        let from = at(1, 0);
        let to = at(2, 0);
        assert_eq!(tier_at_coverages(&[], from), 1);
        assert_eq!(
            segments_from_coverages(&[], from, to),
            vec![(from, to, 1)]
        );
    }

    #[test]
    fn cancel_keeps_historical_tier_for_past_period() {
        let start = at(1, 0);
        let cancel = at(3, 0);
        let period_from = at(1, 0);
        let period_to = at(2, 0);
        let coverages = [coverage(3, start, Some(cancel), None)];

        assert_eq!(tier_at_coverages(&coverages, at(1, 12)), 3);
        assert_eq!(tier_at_coverages(&coverages, cancel), 1);
        assert_eq!(
            segments_from_coverages(&coverages, period_from, period_to),
            vec![(period_from, period_to, 3)]
        );
    }

    #[test]
    fn cancel_splits_period_at_cancelled_at() {
        let start = at(1, 0);
        let cancel = at(2, 0);
        let from = at(1, 0);
        let to = at(3, 0);
        let coverages = [coverage(3, start, Some(cancel), None)];

        assert_eq!(
            segments_from_coverages(&coverages, from, to),
            vec![(from, cancel, 3), (cancel, to, 1)]
        );
    }

    #[test]
    fn mid_period_upgrade_splits_tiers() {
        let basic_start = at(1, 0);
        let pro_start = at(2, 0);
        let from = at(1, 0);
        let to = at(3, 0);
        let coverages = [
            coverage(2, basic_start, None, None),
            coverage(3, pro_start, None, None),
        ];

        assert_eq!(tier_at_coverages(&coverages, at(1, 12)), 2);
        assert_eq!(tier_at_coverages(&coverages, at(2, 12)), 3);
        assert_eq!(
            segments_from_coverages(&coverages, from, to),
            vec![(from, pro_start, 2), (pro_start, to, 3)]
        );
    }

    #[test]
    fn new_package_after_period_does_not_affect_past_segments() {
        let period_from = at(1, 0);
        let period_to = at(2, 0);
        let later_purchase = at(3, 0);
        let coverages = [coverage(4, later_purchase, None, None)];

        assert_eq!(
            segments_from_coverages(&coverages, period_from, period_to),
            vec![(period_from, period_to, 1)]
        );
        assert_eq!(tier_at_coverages(&coverages, at(1, 12)), 1);
        assert_eq!(tier_at_coverages(&coverages, later_purchase), 4);
    }

    #[test]
    fn expires_at_ends_coverage() {
        let start = at(1, 0);
        let expires = at(2, 0);
        let from = at(1, 0);
        let to = at(3, 0);
        let coverages = [coverage(3, start, None, Some(expires))];

        assert_eq!(
            segments_from_coverages(&coverages, from, to),
            vec![(from, expires, 3), (expires, to, 1)]
        );
    }

    #[test]
    fn overlapping_payments_use_max_tier() {
        let from = at(1, 0);
        let to = at(2, 0);
        let coverages = [
            coverage(2, at(1, 0), None, None),
            coverage(4, at(1, 0), None, None),
        ];

        assert_eq!(tier_at_coverages(&coverages, at(1, 12)), 4);
        assert_eq!(
            segments_from_coverages(&coverages, from, to),
            vec![(from, to, 4)]
        );
    }
}
