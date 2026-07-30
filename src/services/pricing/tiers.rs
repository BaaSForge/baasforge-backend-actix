use super::config::{
    TIER1_VOLUME_FACTOR, TIER2_VOLUME_FACTOR, TIER3_VOLUME_FACTOR, TIER4_VOLUME_FACTOR,
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
