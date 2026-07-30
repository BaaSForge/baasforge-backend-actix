use super::config::{
    BASE_PROJECT_PER_HOUR, BASE_STAGE_RUNTIME_PER_HOUR, BASE_SUBSCRIPTION_MONTHLY_USD,
    BASE_UNIT_PER_HOUR,
};
use super::tiers::tier_platform_costs_factor;

pub const HOURS_PER_MONTH: f64 = 30.0 * 24.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlatformResourceRates {
    pub subscription_per_hour: f64,
    pub project_per_hour: f64,
    pub unit_per_hour: f64,
    pub stage_runtime_per_hour: f64,
}

impl PlatformResourceRates {
    pub fn subscription_per_entity_second(self) -> f64 {
        self.subscription_per_hour / 3600.0
    }

    pub fn project_per_entity_second(self) -> f64 {
        self.project_per_hour / 3600.0
    }

    pub fn unit_per_entity_second(self) -> f64 {
        self.unit_per_hour / 3600.0
    }

    pub fn stage_runtime_per_entity_second(self) -> f64 {
        self.stage_runtime_per_hour / 3600.0
    }

    pub fn cost_for_entity_seconds(self, metric: &str, entity_seconds: i64) -> f64 {
        if entity_seconds <= 0 {
            return 0.0;
        }
        let rate = match metric {
            super::metrics::SUBSCRIPTION_METRIC => self.subscription_per_entity_second(),
            super::metrics::PROJECT_METRIC => self.project_per_entity_second(),
            super::metrics::UNIT_METRIC => self.unit_per_entity_second(),
            super::metrics::STAGE_RUNTIME_METRIC => self.stage_runtime_per_entity_second(),
            _ => 0.0,
        };
        entity_seconds as f64 * rate
    }
}

pub fn platform_rates_for_package_tier(tier: i32) -> PlatformResourceRates {
    let factor = tier_platform_costs_factor(tier);
    PlatformResourceRates {
        subscription_per_hour: BASE_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH * factor,
        project_per_hour: BASE_PROJECT_PER_HOUR * factor,
        unit_per_hour: BASE_UNIT_PER_HOUR * factor,
        stage_runtime_per_hour: BASE_STAGE_RUNTIME_PER_HOUR * factor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::{
        TIER1_PLATFORM_COSTS_FACTOR, TIER3_PLATFORM_COSTS_FACTOR, TIER4_PLATFORM_COSTS_FACTOR,
    };

    #[test]
    fn tier1_uses_base_rates() {
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
    fn platform_costs_factor_scales_rates() {
        let t3 = platform_rates_for_package_tier(3);
        let t4 = platform_rates_for_package_tier(4);
        assert_eq!(
            t3.subscription_per_hour,
            BASE_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH * TIER3_PLATFORM_COSTS_FACTOR
        );
        assert_eq!(t3.project_per_hour, BASE_PROJECT_PER_HOUR * TIER3_PLATFORM_COSTS_FACTOR);
        assert_eq!(t3.unit_per_hour, BASE_UNIT_PER_HOUR * TIER3_PLATFORM_COSTS_FACTOR);
        assert_eq!(
            t3.stage_runtime_per_hour,
            BASE_STAGE_RUNTIME_PER_HOUR * TIER3_PLATFORM_COSTS_FACTOR
        );
        assert_eq!(
            t4.subscription_per_hour,
            BASE_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH * TIER4_PLATFORM_COSTS_FACTOR
        );
        assert_eq!(t4.project_per_hour, BASE_PROJECT_PER_HOUR * TIER4_PLATFORM_COSTS_FACTOR);
        assert_eq!(t4.unit_per_hour, BASE_UNIT_PER_HOUR * TIER4_PLATFORM_COSTS_FACTOR);
        assert_eq!(
            t4.stage_runtime_per_hour,
            BASE_STAGE_RUNTIME_PER_HOUR * TIER4_PLATFORM_COSTS_FACTOR
        );
    }
}
