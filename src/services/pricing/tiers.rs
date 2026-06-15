use super::config::{
    TIER1_VOLUME_DISCOUNT, TIER2_VOLUME_DISCOUNT, TIER3_VOLUME_DISCOUNT, TIER4_VOLUME_DISCOUNT,
};

pub fn effective_tier(tier: i32) -> i32 {
    if tier <= 0 { 1 } else { tier }
}

pub fn tier_volume_discount(package_tier: i32) -> f64 {
    match effective_tier(package_tier) {
        1 => TIER1_VOLUME_DISCOUNT,
        2 => TIER2_VOLUME_DISCOUNT,
        3 => TIER3_VOLUME_DISCOUNT,
        _ => TIER4_VOLUME_DISCOUNT,
    }
}
