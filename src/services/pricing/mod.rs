//! Central configuration for usage billing rates (compute, storage, LLM, platform resources).
//!
//! Edit `config.rs` / `llm_prices.rs` to change prices — other files in this folder reference them.

pub mod config;
pub mod compute_storage;
pub mod llm;
pub mod llm_prices;
pub mod metrics;
pub mod platform_resources;
pub mod tiers;

pub use compute_storage::{billing_rates_for_package_tier, BillingRates};
pub use llm::{
    compute_llm_period_cost, customer_rate_per_token, provider_list_rates, LlmPeriodCost,
    LlmUsageSlice, ProviderModelRates, LLM_PLATFORM_MARGIN_FRACTION,
};
pub use metrics::{
    COMPUTE_GB_MS_METRIC, ENTITY_SECOND_PRICE_UNIT, LLM_INPUT_TOKENS_METRIC, LLM_KIND,
    LLM_OUTPUT_TOKENS_METRIC, PLATFORM_KIND, PROJECT_METRIC, STAGE_RUNTIME_METRIC,
    STORAGE_TIER_ACTIVE, STORAGE_TIER_RETAINED, SUBSCRIPTION_METRIC, TOKEN_PRICE_UNIT, UNIT_METRIC,
};
pub use platform_resources::{platform_rates_for_package_tier, PlatformResourceRates};
pub use tiers::{effective_tier, tier_volume_factor};
