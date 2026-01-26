use crate::models::payments::PaymentResponse;
use serde::Serialize;

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
    payments: &[PaymentResponse],
    required_features: &[PaidFeature],
) -> Vec<MissingFeature>
where
    PaidFeature: Clone + PartialEq,
{
    let user_features = features_from_payments(payments);
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
