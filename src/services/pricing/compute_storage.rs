use super::config::{
    TIER1_COMPUTE_PER_GB_MS, TIER1_STORAGE_ACTIVE_PER_GB_MS, TIER1_STORAGE_RETAINED_PER_GB_MS,
    TIER2_COMPUTE_PER_GB_MS, TIER2_STORAGE_ACTIVE_PER_GB_MS, TIER2_STORAGE_RETAINED_PER_GB_MS,
    TIER3_COMPUTE_PER_GB_MS, TIER3_STORAGE_ACTIVE_PER_GB_MS, TIER3_STORAGE_RETAINED_PER_GB_MS,
    TIER4_COMPUTE_PER_GB_MS, TIER4_STORAGE_ACTIVE_PER_GB_MS, TIER4_STORAGE_RETAINED_PER_GB_MS,
};
use super::tiers::effective_tier;

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

pub fn billing_rates_for_package_tier(tier: i32) -> BillingRates {
    match effective_tier(tier) {
        1 => BillingRates {
            compute_gb_ms: TIER1_COMPUTE_PER_GB_MS,
            storage_active: TIER1_STORAGE_ACTIVE_PER_GB_MS,
            storage_retained: TIER1_STORAGE_RETAINED_PER_GB_MS,
        },
        2 => BillingRates {
            compute_gb_ms: TIER2_COMPUTE_PER_GB_MS,
            storage_active: TIER2_STORAGE_ACTIVE_PER_GB_MS,
            storage_retained: TIER2_STORAGE_RETAINED_PER_GB_MS,
        },
        3 => BillingRates {
            compute_gb_ms: TIER3_COMPUTE_PER_GB_MS,
            storage_active: TIER3_STORAGE_ACTIVE_PER_GB_MS,
            storage_retained: TIER3_STORAGE_RETAINED_PER_GB_MS,
        },
        _ => BillingRates {
            compute_gb_ms: TIER4_COMPUTE_PER_GB_MS,
            storage_active: TIER4_STORAGE_ACTIVE_PER_GB_MS,
            storage_retained: TIER4_STORAGE_RETAINED_PER_GB_MS,
        },
    }
}
