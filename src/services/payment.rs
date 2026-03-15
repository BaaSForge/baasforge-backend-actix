use crate::models::packages::Package;
use crate::models::payments::PaymentResponse;
use crate::schema::{packages, payments};
use chrono::Utc;
use diesel::prelude::*;
use serde::Serialize;
use uuid::Uuid;

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
        .filter(not_expired)
        .order(payments::id.desc())
        .select((PaymentResponse::as_select(), Package::as_select()))
        .load(conn)?;

    let largest_tier: i32 = payments::table
        .filter(payments::user_id.eq(user_id))
        .filter(payments::is_success.eq(true))
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

pub const PACKAGE_PAID_FEATURES: [(&str, &[PaidFeature]); 3] = [
    ("free-trial", &[]),
    ("basic", &[PaidFeature::LotsOfProjects]),
    (
        "pro",
        &[
            PaidFeature::LotsOfProjects,
            PaidFeature::MonitoringDashboard,
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
