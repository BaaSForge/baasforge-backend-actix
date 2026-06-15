use super::config::{
    TIER1_PROJECT_PER_HOUR, TIER1_STAGE_RUNTIME_PER_HOUR, TIER1_SUBSCRIPTION_MONTHLY_USD,
    TIER1_UNIT_PER_HOUR, TIER2_PROJECT_PER_HOUR, TIER2_STAGE_RUNTIME_PER_HOUR,
    TIER2_SUBSCRIPTION_MONTHLY_USD, TIER2_UNIT_PER_HOUR, TIER3_PROJECT_PER_HOUR,
    TIER3_STAGE_RUNTIME_PER_HOUR, TIER3_SUBSCRIPTION_MONTHLY_USD, TIER3_UNIT_PER_HOUR,
    TIER4_PROJECT_PER_HOUR, TIER4_STAGE_RUNTIME_PER_HOUR, TIER4_SUBSCRIPTION_MONTHLY_USD,
    TIER4_UNIT_PER_HOUR,
};
use super::tiers::effective_tier;

const HOURS_PER_MONTH: f64 = 30.0 * 24.0;

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
    match effective_tier(tier) {
        1 => PlatformResourceRates {
            subscription_per_hour: TIER1_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH,
            project_per_hour: TIER1_PROJECT_PER_HOUR,
            unit_per_hour: TIER1_UNIT_PER_HOUR,
            stage_runtime_per_hour: TIER1_STAGE_RUNTIME_PER_HOUR,
        },
        2 => PlatformResourceRates {
            subscription_per_hour: TIER2_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH,
            project_per_hour: TIER2_PROJECT_PER_HOUR,
            unit_per_hour: TIER2_UNIT_PER_HOUR,
            stage_runtime_per_hour: TIER2_STAGE_RUNTIME_PER_HOUR,
        },
        3 => PlatformResourceRates {
            subscription_per_hour: TIER3_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH,
            project_per_hour: TIER3_PROJECT_PER_HOUR,
            unit_per_hour: TIER3_UNIT_PER_HOUR,
            stage_runtime_per_hour: TIER3_STAGE_RUNTIME_PER_HOUR,
        },
        _ => PlatformResourceRates {
            subscription_per_hour: TIER4_SUBSCRIPTION_MONTHLY_USD / HOURS_PER_MONTH,
            project_per_hour: TIER4_PROJECT_PER_HOUR,
            unit_per_hour: TIER4_UNIT_PER_HOUR,
            stage_runtime_per_hour: TIER4_STAGE_RUNTIME_PER_HOUR,
        },
    }
}
