use super::config::{
    BASE_COMPUTE_PER_GB_MS, BASE_STORAGE_ACTIVE_PER_GB_MS, BASE_STORAGE_RETAINED_PER_GB_MS,
};
use super::tiers::tier_volume_factor;

const BYTES_PER_GB: f64 = 1024.0 * 1024.0 * 1024.0;
const MS_PER_HOUR: f64 = 3_600_000.0;

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

    pub fn storage_active_cost(self, byte_hours: f64) -> f64 {
        byte_hours_to_gb_ms(byte_hours) * self.storage_active
    }

    pub fn storage_retained_cost(self, byte_hours: f64) -> f64 {
        byte_hours_to_gb_ms(byte_hours) * self.storage_retained
    }
}

pub fn byte_hours_to_gb_ms(byte_hours: f64) -> f64 {
    byte_hours * MS_PER_HOUR / BYTES_PER_GB
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

    #[test]
    fn one_gib_one_hour_is_3_6m_gb_ms() {
        let byte_hours = BYTES_PER_GB * 1.0;
        assert!((byte_hours_to_gb_ms(byte_hours) - MS_PER_HOUR).abs() < 1e-6);
        let rates = billing_rates_for_package_tier(1);
        assert!((rates.storage_active_cost(byte_hours) - MS_PER_HOUR * BASE_STORAGE_ACTIVE_PER_GB_MS).abs() < 1e-15);
    }
}
