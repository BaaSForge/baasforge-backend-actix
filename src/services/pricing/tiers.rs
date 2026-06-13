pub fn effective_tier(tier: i32) -> i32 {
    if tier <= 0 { 1 } else { tier }
}

/// Paid-package volume discount applied on top of list rates (compute, storage, LLM).
pub fn tier_volume_discount(package_tier: i32) -> f64 {
    match effective_tier(package_tier) {
        1 => 1.0,
        2 => 0.8,
        _ => 0.6,
    }
}
