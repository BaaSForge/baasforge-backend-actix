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
