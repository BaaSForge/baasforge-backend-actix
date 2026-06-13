//! Central configuration for usage billing rates (compute, storage, LLM).
//!
//! Edit files in this folder to change prices — billing logic lives in `services::billing`.

pub mod compute_storage;
pub mod llm;
pub mod metrics;
pub mod tiers;

pub use compute_storage::{billing_rates_for_package_tier, BillingRates};
pub use llm::{
    compute_llm_period_cost, customer_rate_per_token, provider_list_rates, LlmPeriodCost,
    LlmUsageSlice, ProviderModelRates, LLM_PLATFORM_MARGIN_FRACTION,
};
pub use metrics::{
    COMPUTE_GB_MS_METRIC, LLM_INPUT_TOKENS_METRIC, LLM_KIND, LLM_OUTPUT_TOKENS_METRIC,
    STORAGE_TIER_ACTIVE, STORAGE_TIER_RETAINED, TOKEN_PRICE_UNIT,
};
pub use tiers::{effective_tier, tier_volume_discount};
