use super::config::{
    BASE_COMPUTE_PER_GB_MS, BASE_STORAGE_ACTIVE_PER_GB_MS, BASE_STORAGE_RETAINED_PER_GB_MS,
};
use super::tiers::tier_volume_factor;

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
    let factor = tier_volume_factor(tier);
    BillingRates {
        compute_gb_ms: BASE_COMPUTE_PER_GB_MS * factor,
        storage_active: BASE_STORAGE_ACTIVE_PER_GB_MS * factor,
        storage_retained: BASE_STORAGE_RETAINED_PER_GB_MS * factor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::{TIER3_VOLUME_FACTOR, TIER4_VOLUME_FACTOR};

    #[test]
    fn tier1_uses_base_rates() {
        let rates = billing_rates_for_package_tier(1);
        assert_eq!(rates.compute_gb_ms, BASE_COMPUTE_PER_GB_MS);
        assert_eq!(rates.storage_active, BASE_STORAGE_ACTIVE_PER_GB_MS);
        assert_eq!(rates.storage_retained, BASE_STORAGE_RETAINED_PER_GB_MS);
    }

    #[test]
    fn volume_factor_scales_compute_and_storage() {
        let t3 = billing_rates_for_package_tier(3);
        let t4 = billing_rates_for_package_tier(4);
        assert_eq!(t3.compute_gb_ms, BASE_COMPUTE_PER_GB_MS * TIER3_VOLUME_FACTOR);
        assert_eq!(t3.storage_active, BASE_STORAGE_ACTIVE_PER_GB_MS * TIER3_VOLUME_FACTOR);
        assert_eq!(t3.storage_retained, BASE_STORAGE_RETAINED_PER_GB_MS * TIER3_VOLUME_FACTOR);
        assert_eq!(t4.compute_gb_ms, BASE_COMPUTE_PER_GB_MS * TIER4_VOLUME_FACTOR);
        assert_eq!(t4.storage_active, BASE_STORAGE_ACTIVE_PER_GB_MS * TIER4_VOLUME_FACTOR);
        assert_eq!(t4.storage_retained, BASE_STORAGE_RETAINED_PER_GB_MS * TIER4_VOLUME_FACTOR);
    }
}
