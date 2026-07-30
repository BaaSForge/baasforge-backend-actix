use super::config::LLM_PLATFORM_MARGIN;
use super::llm_prices::{
    ANTHROPIC_CLAUDE_HAIKU_4_5_INPUT_PER_M, ANTHROPIC_CLAUDE_HAIKU_4_5_OUTPUT_PER_M,
    ANTHROPIC_CLAUDE_OPUS_4_6_INPUT_PER_M, ANTHROPIC_CLAUDE_OPUS_4_6_OUTPUT_PER_M,
    ANTHROPIC_CLAUDE_OPUS_4_7_INPUT_PER_M, ANTHROPIC_CLAUDE_OPUS_4_7_OUTPUT_PER_M,
    ANTHROPIC_CLAUDE_OPUS_4_8_INPUT_PER_M, ANTHROPIC_CLAUDE_OPUS_4_8_OUTPUT_PER_M,
    ANTHROPIC_CLAUDE_SONNET_4_5_INPUT_PER_M, ANTHROPIC_CLAUDE_SONNET_4_5_OUTPUT_PER_M,
    ANTHROPIC_CLAUDE_SONNET_4_6_INPUT_PER_M, ANTHROPIC_CLAUDE_SONNET_4_6_OUTPUT_PER_M,
    FALLBACK_ANTHROPIC_INPUT_PER_M, FALLBACK_ANTHROPIC_OUTPUT_PER_M,
    FALLBACK_GEMINI_INPUT_PER_M, FALLBACK_GEMINI_OUTPUT_PER_M, FALLBACK_OPENAI_INPUT_PER_M,
    FALLBACK_OPENAI_OUTPUT_PER_M, FALLBACK_UNKNOWN_PROVIDER_INPUT_PER_M,
    FALLBACK_UNKNOWN_PROVIDER_OUTPUT_PER_M, GEMINI_2_5_FLASH_INPUT_PER_M,
    GEMINI_2_5_FLASH_LITE_INPUT_PER_M, GEMINI_2_5_FLASH_LITE_OUTPUT_PER_M,
    GEMINI_2_5_FLASH_OUTPUT_PER_M, GEMINI_2_5_PRO_INPUT_PER_M, GEMINI_2_5_PRO_OUTPUT_PER_M,
    GEMINI_3_1_FLASH_LITE_INPUT_PER_M, GEMINI_3_1_FLASH_LITE_OUTPUT_PER_M,
    GEMINI_3_1_PRO_INPUT_PER_M, GEMINI_3_1_PRO_OUTPUT_PER_M, GEMINI_3_5_FLASH_INPUT_PER_M,
    GEMINI_3_5_FLASH_OUTPUT_PER_M, GEMINI_3_FLASH_INPUT_PER_M, GEMINI_3_FLASH_OUTPUT_PER_M,
    GEMINI_3_PRO_INPUT_PER_M, GEMINI_3_PRO_OUTPUT_PER_M, OPENAI_GPT_4_1_INPUT_PER_M,
    OPENAI_GPT_4_1_MINI_INPUT_PER_M, OPENAI_GPT_4_1_MINI_OUTPUT_PER_M,
    OPENAI_GPT_4_1_NANO_INPUT_PER_M, OPENAI_GPT_4_1_NANO_OUTPUT_PER_M,
    OPENAI_GPT_4_1_OUTPUT_PER_M, OPENAI_GPT_4O_INPUT_PER_M, OPENAI_GPT_4O_MINI_INPUT_PER_M,
    OPENAI_GPT_4O_MINI_OUTPUT_PER_M, OPENAI_GPT_4O_OUTPUT_PER_M, OPENAI_GPT_5_4_INPUT_PER_M,
    OPENAI_GPT_5_4_MINI_INPUT_PER_M, OPENAI_GPT_5_4_MINI_OUTPUT_PER_M,
    OPENAI_GPT_5_4_NANO_INPUT_PER_M, OPENAI_GPT_5_4_NANO_OUTPUT_PER_M,
    OPENAI_GPT_5_4_OUTPUT_PER_M, OPENAI_GPT_5_5_INPUT_PER_M, OPENAI_GPT_5_5_OUTPUT_PER_M,
    OPENAI_O3_INPUT_PER_M, OPENAI_O3_MINI_INPUT_PER_M, OPENAI_O3_MINI_OUTPUT_PER_M,
    OPENAI_O3_OUTPUT_PER_M, OPENAI_O4_MINI_INPUT_PER_M, OPENAI_O4_MINI_OUTPUT_PER_M,
};
use super::metrics::{LLM_INPUT_TOKENS_METRIC, LLM_OUTPUT_TOKENS_METRIC};
use super::tiers::tier_volume_factor;

pub use super::config::LLM_PLATFORM_MARGIN as LLM_PLATFORM_MARGIN_FRACTION;

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
    per_million / 1_000_000.0 * (1.0 + LLM_PLATFORM_MARGIN) * tier_volume_factor(package_tier)
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
        let rate =
            customer_rate_per_token(&slice.provider, &slice.model, &slice.metric, package_tier);
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

    LlmPeriodCost { sum_input_tokens, sum_output_tokens, input_cost, output_cost }
}

fn provider_model_rates(provider: &str, model: &str) -> Option<ProviderModelRates> {
    Some(match (provider, model) {
        ("openai", "gpt-4o-mini") => rates(OPENAI_GPT_4O_MINI_INPUT_PER_M, OPENAI_GPT_4O_MINI_OUTPUT_PER_M),
        ("openai", "gpt-4o") => rates(OPENAI_GPT_4O_INPUT_PER_M, OPENAI_GPT_4O_OUTPUT_PER_M),
        ("openai", "gpt-4.1") => rates(OPENAI_GPT_4_1_INPUT_PER_M, OPENAI_GPT_4_1_OUTPUT_PER_M),
        ("openai", "gpt-4.1-mini") => rates(OPENAI_GPT_4_1_MINI_INPUT_PER_M, OPENAI_GPT_4_1_MINI_OUTPUT_PER_M),
        ("openai", "gpt-4.1-nano") => rates(OPENAI_GPT_4_1_NANO_INPUT_PER_M, OPENAI_GPT_4_1_NANO_OUTPUT_PER_M),
        ("openai", "o3-mini") => rates(OPENAI_O3_MINI_INPUT_PER_M, OPENAI_O3_MINI_OUTPUT_PER_M),
        ("openai", "o3") => rates(OPENAI_O3_INPUT_PER_M, OPENAI_O3_OUTPUT_PER_M),
        ("openai", "o4-mini") => rates(OPENAI_O4_MINI_INPUT_PER_M, OPENAI_O4_MINI_OUTPUT_PER_M),
        ("openai", "gpt-5.4-nano") => rates(OPENAI_GPT_5_4_NANO_INPUT_PER_M, OPENAI_GPT_5_4_NANO_OUTPUT_PER_M),
        ("openai", "gpt-5.4-mini") => rates(OPENAI_GPT_5_4_MINI_INPUT_PER_M, OPENAI_GPT_5_4_MINI_OUTPUT_PER_M),
        ("openai", "gpt-5.4") => rates(OPENAI_GPT_5_4_INPUT_PER_M, OPENAI_GPT_5_4_OUTPUT_PER_M),
        ("openai", "gpt-5.5") => rates(OPENAI_GPT_5_5_INPUT_PER_M, OPENAI_GPT_5_5_OUTPUT_PER_M),

        ("anthropic", "claude-haiku-4-5") => rates(ANTHROPIC_CLAUDE_HAIKU_4_5_INPUT_PER_M, ANTHROPIC_CLAUDE_HAIKU_4_5_OUTPUT_PER_M),
        ("anthropic", "claude-haiku-4-5-20251001") => rates(ANTHROPIC_CLAUDE_HAIKU_4_5_INPUT_PER_M, ANTHROPIC_CLAUDE_HAIKU_4_5_OUTPUT_PER_M),
        ("anthropic", "claude-sonnet-4-6") => rates(ANTHROPIC_CLAUDE_SONNET_4_6_INPUT_PER_M, ANTHROPIC_CLAUDE_SONNET_4_6_OUTPUT_PER_M),
        ("anthropic", "claude-sonnet-4-5") => rates(ANTHROPIC_CLAUDE_SONNET_4_5_INPUT_PER_M, ANTHROPIC_CLAUDE_SONNET_4_5_OUTPUT_PER_M),
        ("anthropic", "claude-opus-4-6") => rates(ANTHROPIC_CLAUDE_OPUS_4_6_INPUT_PER_M, ANTHROPIC_CLAUDE_OPUS_4_6_OUTPUT_PER_M),
        ("anthropic", "claude-opus-4-7") => rates(ANTHROPIC_CLAUDE_OPUS_4_7_INPUT_PER_M, ANTHROPIC_CLAUDE_OPUS_4_7_OUTPUT_PER_M),
        ("anthropic", "claude-opus-4-8") => rates(ANTHROPIC_CLAUDE_OPUS_4_8_INPUT_PER_M, ANTHROPIC_CLAUDE_OPUS_4_8_OUTPUT_PER_M),

        ("gemini", "gemini-2.5-flash-lite") => rates(GEMINI_2_5_FLASH_LITE_INPUT_PER_M, GEMINI_2_5_FLASH_LITE_OUTPUT_PER_M),
        ("gemini", "gemini-2.5-flash") => rates(GEMINI_2_5_FLASH_INPUT_PER_M, GEMINI_2_5_FLASH_OUTPUT_PER_M),
        ("gemini", "gemini-2.5-pro") => rates(GEMINI_2_5_PRO_INPUT_PER_M, GEMINI_2_5_PRO_OUTPUT_PER_M),
        ("gemini", "gemini-3-flash") => rates(GEMINI_3_FLASH_INPUT_PER_M, GEMINI_3_FLASH_OUTPUT_PER_M),
        ("gemini", "gemini-3.5-flash") => rates(GEMINI_3_5_FLASH_INPUT_PER_M, GEMINI_3_5_FLASH_OUTPUT_PER_M),
        ("gemini", "gemini-3.1-flash-lite-preview") => rates(GEMINI_3_1_FLASH_LITE_INPUT_PER_M, GEMINI_3_1_FLASH_LITE_OUTPUT_PER_M),
        ("gemini", "gemini-3-pro-preview") => rates(GEMINI_3_PRO_INPUT_PER_M, GEMINI_3_PRO_OUTPUT_PER_M),
        ("gemini", "gemini-3.1-pro-preview") => rates(GEMINI_3_1_PRO_INPUT_PER_M, GEMINI_3_1_PRO_OUTPUT_PER_M),

        _ => return None,
    })
}

fn provider_default_rates(provider: &str) -> ProviderModelRates {
    match provider {
        "openai" => rates(FALLBACK_OPENAI_INPUT_PER_M, FALLBACK_OPENAI_OUTPUT_PER_M),
        "anthropic" => rates(FALLBACK_ANTHROPIC_INPUT_PER_M, FALLBACK_ANTHROPIC_OUTPUT_PER_M),
        "gemini" => rates(FALLBACK_GEMINI_INPUT_PER_M, FALLBACK_GEMINI_OUTPUT_PER_M),
        _ => rates(FALLBACK_UNKNOWN_PROVIDER_INPUT_PER_M, FALLBACK_UNKNOWN_PROVIDER_OUTPUT_PER_M),
    }
}

const fn rates(input_per_million: f64, output_per_million: f64) -> ProviderModelRates {
    ProviderModelRates { input_per_million, output_per_million }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt_4o_mini_tier1_with_margin() {
        let rate = customer_rate_per_token("openai", "gpt-4o-mini", LLM_INPUT_TOKENS_METRIC, 1);
        let expected = OPENAI_GPT_4O_MINI_INPUT_PER_M / 1_000_000.0 * (1.0 + LLM_PLATFORM_MARGIN);
        assert_eq!(rate, expected);
    }

    #[test]
    fn volume_factor_scales_rate() {
        let t3 = customer_rate_per_token("anthropic", "claude-sonnet-4-6", LLM_OUTPUT_TOKENS_METRIC, 3);
        let expected = ANTHROPIC_CLAUDE_SONNET_4_6_OUTPUT_PER_M / 1_000_000.0
            * (1.0 + LLM_PLATFORM_MARGIN)
            * super::super::config::TIER3_VOLUME_FACTOR;
        assert_eq!(t3, expected);
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
        let margin = 1.0 + LLM_PLATFORM_MARGIN;
        assert_eq!(cost.input_cost, OPENAI_GPT_4O_MINI_INPUT_PER_M * margin);
        assert_eq!(cost.output_cost, OPENAI_GPT_4O_MINI_OUTPUT_PER_M * 0.5 * margin);
    }

    #[test]
    fn normalizes_gemini_models_prefix() {
        assert_eq!(
            provider_list_rates("gemini", "models/gemini-2.5-flash"),
            provider_list_rates("gemini", "gemini-2.5-flash")
        );
    }
}
