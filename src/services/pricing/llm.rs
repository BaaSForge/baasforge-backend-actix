//! Provider list prices (USD per 1M tokens) and customer rates for LLM billing.
//!
//! Margin model: platforms like OpenRouter pass through provider token rates and charge
//! ~5.5% on credit purchase; inference gateways often add ~5–10% on usage. We apply a
//! single configurable markup on provider list price, then package-tier volume discount.

use super::metrics::{LLM_INPUT_TOKENS_METRIC, LLM_OUTPUT_TOKENS_METRIC};
use super::tiers::tier_volume_discount;

/// Markup on provider list price (not env). 0.08 = 8% — between OpenRouter credit fee (~5.5%)
/// and typical B2B gateway inference markup (~10%).
pub const LLM_PLATFORM_MARGIN_FRACTION: f64 = 0.08;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderModelRates {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmUsageSlice {
    pub provider: String,
    pub model: String,
    pub metric: String,
    pub quantity: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LlmPeriodCost {
    pub sum_input_tokens: i64,
    pub sum_output_tokens: i64,
    pub input_cost: f64,
    pub output_cost: f64,
}

impl LlmPeriodCost {
    pub fn total_cost(self) -> f64 {
        self.input_cost + self.output_cost
    }
}

pub fn normalize_provider(provider: &str) -> String {
    provider.trim().to_lowercase()
}

pub fn normalize_model_id(model: &str) -> String {
    model.trim().trim_start_matches("models/").to_lowercase()
}

pub fn provider_list_rates(provider: &str, model: &str) -> ProviderModelRates {
    let provider = normalize_provider(provider);
    let model = normalize_model_id(model);
    provider_model_rates(&provider, &model).unwrap_or_else(|| provider_default_rates(&provider))
}

pub fn customer_rate_per_token(
    provider: &str,
    model: &str,
    metric: &str,
    package_tier: i32,
) -> f64 {
    let list = provider_list_rates(provider, model);
    let per_million = match metric {
        LLM_INPUT_TOKENS_METRIC => list.input_per_million,
        LLM_OUTPUT_TOKENS_METRIC => list.output_per_million,
        _ => 0.0,
    };
    per_million / 1_000_000.0
        * (1.0 + LLM_PLATFORM_MARGIN_FRACTION)
        * tier_volume_discount(package_tier)
}

pub fn compute_llm_period_cost(slices: &[LlmUsageSlice], package_tier: i32) -> LlmPeriodCost {
    let mut sum_input_tokens = 0_i64;
    let mut sum_output_tokens = 0_i64;
    let mut input_cost = 0.0_f64;
    let mut output_cost = 0.0_f64;

    for slice in slices {
        if slice.quantity <= 0 {
            continue;
        }
        let rate = customer_rate_per_token(&slice.provider, &slice.model, &slice.metric, package_tier);
        let cost = slice.quantity as f64 * rate;
        match slice.metric.as_str() {
            LLM_INPUT_TOKENS_METRIC => {
                sum_input_tokens += slice.quantity;
                input_cost += cost;
            }
            LLM_OUTPUT_TOKENS_METRIC => {
                sum_output_tokens += slice.quantity;
                output_cost += cost;
            }
            _ => {}
        }
    }

    LlmPeriodCost {
        sum_input_tokens,
        sum_output_tokens,
        input_cost,
        output_cost,
    }
}

fn provider_model_rates(provider: &str, model: &str) -> Option<ProviderModelRates> {
    Some(match (provider, model) {
        // OpenAI — https://openai.com/api/pricing/
        ("openai", "gpt-4o-mini") => rates(0.15, 0.60),
        ("openai", "gpt-4o") => rates(2.50, 10.00),
        ("openai", "gpt-4.1") => rates(2.00, 8.00),
        ("openai", "gpt-4.1-mini") => rates(0.40, 1.60),
        ("openai", "gpt-4.1-nano") => rates(0.10, 0.40),
        ("openai", "o3-mini") => rates(1.10, 4.40),
        ("openai", "o3") => rates(2.00, 8.00),
        ("openai", "o4-mini") => rates(1.10, 4.40),
        ("openai", "gpt-5.4-nano") => rates(0.20, 1.25),
        ("openai", "gpt-5.4-mini") => rates(0.75, 4.50),
        ("openai", "gpt-5.4") => rates(2.50, 15.00),
        ("openai", "gpt-5.5") => rates(5.00, 30.00),

        // Anthropic — https://platform.claude.com/docs/en/about-claude/models/overview
        ("anthropic", "claude-haiku-4-5") => rates(1.00, 5.00),
        ("anthropic", "claude-haiku-4-5-20251001") => rates(1.00, 5.00),
        ("anthropic", "claude-sonnet-4-6") => rates(3.00, 15.00),
        ("anthropic", "claude-sonnet-4-5") => rates(3.00, 15.00),
        ("anthropic", "claude-opus-4-6") => rates(5.00, 25.00),
        ("anthropic", "claude-opus-4-7") => rates(5.00, 25.00),
        ("anthropic", "claude-opus-4-8") => rates(5.00, 25.00),

        // Google Gemini — https://ai.google.dev/gemini-api/docs/pricing (≤200K context tier)
        ("gemini", "gemini-2.5-flash-lite") => rates(0.10, 0.40),
        ("gemini", "gemini-2.5-flash") => rates(0.30, 2.50),
        ("gemini", "gemini-2.5-pro") => rates(1.25, 10.00),
        ("gemini", "gemini-3-flash") => rates(0.50, 3.00),
        ("gemini", "gemini-3.5-flash") => rates(1.50, 9.00),
        ("gemini", "gemini-3.1-flash-lite-preview") => rates(0.25, 1.50),
        ("gemini", "gemini-3-pro-preview") => rates(2.00, 12.00),
        ("gemini", "gemini-3.1-pro-preview") => rates(2.00, 12.00),

        _ => return None,
    })
}

fn provider_default_rates(provider: &str) -> ProviderModelRates {
    match provider {
        "openai" => rates(2.50, 10.00),
        "anthropic" => rates(3.00, 15.00),
        "gemini" => rates(0.30, 2.50),
        _ => rates(3.00, 15.00),
    }
}

const fn rates(input_per_million: f64, output_per_million: f64) -> ProviderModelRates {
    ProviderModelRates {
        input_per_million,
        output_per_million,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt_4o_mini_tier1_with_margin() {
        let rate = customer_rate_per_token("openai", "gpt-4o-mini", LLM_INPUT_TOKENS_METRIC, 1);
        let expected = 0.15 / 1_000_000.0 * 1.08;
        assert!((rate - expected).abs() < 1e-15);
    }

    #[test]
    fn tier_discount_reduces_rate() {
        let t1 = customer_rate_per_token("anthropic", "claude-sonnet-4-6", LLM_OUTPUT_TOKENS_METRIC, 1);
        let t3 = customer_rate_per_token("anthropic", "claude-sonnet-4-6", LLM_OUTPUT_TOKENS_METRIC, 3);
        assert!((t3 / t1 - 0.6).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_uses_provider_default() {
        let known = provider_list_rates("openai", "gpt-4o");
        let unknown = provider_list_rates("openai", "some-future-model");
        assert_eq!(known, unknown);
    }

    #[test]
    fn period_cost_sums_by_model() {
        let cost = compute_llm_period_cost(
            &[
                LlmUsageSlice {
                    provider: "openai".into(),
                    model: "gpt-4o-mini".into(),
                    metric: LLM_INPUT_TOKENS_METRIC.into(),
                    quantity: 1_000_000,
                },
                LlmUsageSlice {
                    provider: "openai".into(),
                    model: "gpt-4o-mini".into(),
                    metric: LLM_OUTPUT_TOKENS_METRIC.into(),
                    quantity: 500_000,
                },
            ],
            1,
        );
        assert_eq!(cost.sum_input_tokens, 1_000_000);
        assert_eq!(cost.sum_output_tokens, 500_000);
        assert!((cost.input_cost - 0.15 * 1.08).abs() < 1e-9);
        assert!((cost.output_cost - 0.30 * 1.08).abs() < 1e-9);
    }

    #[test]
    fn normalizes_gemini_models_prefix() {
        assert_eq!(
            provider_list_rates("gemini", "models/gemini-2.5-flash"),
            provider_list_rates("gemini", "gemini-2.5-flash")
        );
    }
}
