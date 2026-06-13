use super::tiers::{effective_tier};

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
            subscription_per_hour: 0.0,
            project_per_hour: 0.002,
            unit_per_hour: 0.0002,
            stage_runtime_per_hour: 0.01,
        },
        2 => PlatformResourceRates {
            subscription_per_hour: 10.0 / (30.0 * 24.0),
            project_per_hour: 0.0016,
            unit_per_hour: 0.00016,
            stage_runtime_per_hour: 0.008,
        },
        _ => PlatformResourceRates {
            subscription_per_hour: 30.0 / (30.0 * 24.0),
            project_per_hour: 0.0012,
            unit_per_hour: 0.00012,
            stage_runtime_per_hour: 0.006,
        },
    }
}
