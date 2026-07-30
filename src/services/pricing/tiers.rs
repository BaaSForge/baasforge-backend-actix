use super::config::{
    TIER1_PLATFORM_COSTS_FACTOR, TIER1_PROJECTS_COUNT_LIMIT, TIER1_STAGE_COUNT_LIMIT,
    TIER1_VOLUME_FACTOR, TIER2_PLATFORM_COSTS_FACTOR, TIER2_PROJECTS_COUNT_LIMIT,
    TIER2_STAGE_COUNT_LIMIT, TIER2_VOLUME_FACTOR, TIER3_PLATFORM_COSTS_FACTOR,
    TIER3_PROJECTS_COUNT_LIMIT, TIER3_STAGE_COUNT_LIMIT, TIER3_VOLUME_FACTOR,
    TIER4_PLATFORM_COSTS_FACTOR, TIER4_PROJECTS_COUNT_LIMIT, TIER4_STAGE_COUNT_LIMIT,
    TIER4_VOLUME_FACTOR,
};

pub fn effective_tier(tier: i32) -> i32 {
    if tier <= 0 { 1 } else { tier }
}

pub fn tier_volume_factor(package_tier: i32) -> f64 {
    match effective_tier(package_tier) {
        1 => TIER1_VOLUME_FACTOR,
        2 => TIER2_VOLUME_FACTOR,
        3 => TIER3_VOLUME_FACTOR,
        4 => TIER4_VOLUME_FACTOR,
        _ => TIER1_VOLUME_FACTOR,
    }
}

pub fn tier_platform_costs_factor(package_tier: i32) -> f64 {
    match effective_tier(package_tier) {
        1 => TIER1_PLATFORM_COSTS_FACTOR,
        2 => TIER2_PLATFORM_COSTS_FACTOR,
        3 => TIER3_PLATFORM_COSTS_FACTOR,
        4 => TIER4_PLATFORM_COSTS_FACTOR,
        _ => TIER1_PLATFORM_COSTS_FACTOR,
    }
}

pub fn tier_projects_count_limit(package_tier: i32) -> u32 {
    match effective_tier(package_tier) {
        1 => TIER1_PROJECTS_COUNT_LIMIT,
        2 => TIER2_PROJECTS_COUNT_LIMIT,
        3 => TIER3_PROJECTS_COUNT_LIMIT,
        4 => TIER4_PROJECTS_COUNT_LIMIT,
        _ => TIER1_PROJECTS_COUNT_LIMIT,
    }
}

pub fn tier_stage_count_limit(package_tier: i32) -> u32 {
    match effective_tier(package_tier) {
        1 => TIER1_STAGE_COUNT_LIMIT,
        2 => TIER2_STAGE_COUNT_LIMIT,
        3 => TIER3_STAGE_COUNT_LIMIT,
        4 => TIER4_STAGE_COUNT_LIMIT,
        _ => TIER1_STAGE_COUNT_LIMIT,
    }
}
