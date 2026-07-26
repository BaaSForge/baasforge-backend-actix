// ── LLM ──────────────────────────────────────────────────────────────────────

/// Markup applied on top of every provider list price.
pub const LLM_PLATFORM_MARGIN: f64 = 2.0;

// OpenAI list prices (USD / 1M tokens — input / output)
pub const OPENAI_GPT_4O_MINI_INPUT_PER_M: f64 = 0.15;
pub const OPENAI_GPT_4O_MINI_OUTPUT_PER_M: f64 = 0.60;
pub const OPENAI_GPT_4O_INPUT_PER_M: f64 = 2.50;
pub const OPENAI_GPT_4O_OUTPUT_PER_M: f64 = 10.00;
pub const OPENAI_GPT_4_1_INPUT_PER_M: f64 = 2.00;
pub const OPENAI_GPT_4_1_OUTPUT_PER_M: f64 = 8.00;
pub const OPENAI_GPT_4_1_MINI_INPUT_PER_M: f64 = 0.40;
pub const OPENAI_GPT_4_1_MINI_OUTPUT_PER_M: f64 = 1.60;
pub const OPENAI_GPT_4_1_NANO_INPUT_PER_M: f64 = 0.10;
pub const OPENAI_GPT_4_1_NANO_OUTPUT_PER_M: f64 = 0.40;
pub const OPENAI_O3_MINI_INPUT_PER_M: f64 = 1.10;
pub const OPENAI_O3_MINI_OUTPUT_PER_M: f64 = 4.40;
pub const OPENAI_O3_INPUT_PER_M: f64 = 2.00;
pub const OPENAI_O3_OUTPUT_PER_M: f64 = 8.00;
pub const OPENAI_O4_MINI_INPUT_PER_M: f64 = 1.10;
pub const OPENAI_O4_MINI_OUTPUT_PER_M: f64 = 4.40;
pub const OPENAI_GPT_5_4_NANO_INPUT_PER_M: f64 = 0.20;
pub const OPENAI_GPT_5_4_NANO_OUTPUT_PER_M: f64 = 1.25;
pub const OPENAI_GPT_5_4_MINI_INPUT_PER_M: f64 = 0.75;
pub const OPENAI_GPT_5_4_MINI_OUTPUT_PER_M: f64 = 4.50;
pub const OPENAI_GPT_5_4_INPUT_PER_M: f64 = 2.50;
pub const OPENAI_GPT_5_4_OUTPUT_PER_M: f64 = 15.00;
pub const OPENAI_GPT_5_5_INPUT_PER_M: f64 = 5.00;
pub const OPENAI_GPT_5_5_OUTPUT_PER_M: f64 = 30.00;

// Anthropic list prices (USD / 1M tokens — input / output)
pub const ANTHROPIC_CLAUDE_HAIKU_4_5_INPUT_PER_M: f64 = 1.00;
pub const ANTHROPIC_CLAUDE_HAIKU_4_5_OUTPUT_PER_M: f64 = 5.00;
pub const ANTHROPIC_CLAUDE_SONNET_4_5_INPUT_PER_M: f64 = 3.00;
pub const ANTHROPIC_CLAUDE_SONNET_4_5_OUTPUT_PER_M: f64 = 15.00;
pub const ANTHROPIC_CLAUDE_SONNET_4_6_INPUT_PER_M: f64 = 3.00;
pub const ANTHROPIC_CLAUDE_SONNET_4_6_OUTPUT_PER_M: f64 = 15.00;
pub const ANTHROPIC_CLAUDE_OPUS_4_6_INPUT_PER_M: f64 = 5.00;
pub const ANTHROPIC_CLAUDE_OPUS_4_6_OUTPUT_PER_M: f64 = 25.00;
pub const ANTHROPIC_CLAUDE_OPUS_4_7_INPUT_PER_M: f64 = 5.00;
pub const ANTHROPIC_CLAUDE_OPUS_4_7_OUTPUT_PER_M: f64 = 25.00;
pub const ANTHROPIC_CLAUDE_OPUS_4_8_INPUT_PER_M: f64 = 5.00;
pub const ANTHROPIC_CLAUDE_OPUS_4_8_OUTPUT_PER_M: f64 = 25.00;

// Google Gemini list prices (USD / 1M tokens — input / output)
pub const GEMINI_2_5_FLASH_LITE_INPUT_PER_M: f64 = 0.10;
pub const GEMINI_2_5_FLASH_LITE_OUTPUT_PER_M: f64 = 0.40;
pub const GEMINI_2_5_FLASH_INPUT_PER_M: f64 = 0.30;
pub const GEMINI_2_5_FLASH_OUTPUT_PER_M: f64 = 2.50;
pub const GEMINI_2_5_PRO_INPUT_PER_M: f64 = 1.25;
pub const GEMINI_2_5_PRO_OUTPUT_PER_M: f64 = 10.00;
pub const GEMINI_3_FLASH_INPUT_PER_M: f64 = 0.50;
pub const GEMINI_3_FLASH_OUTPUT_PER_M: f64 = 3.00;
pub const GEMINI_3_5_FLASH_INPUT_PER_M: f64 = 1.50;
pub const GEMINI_3_5_FLASH_OUTPUT_PER_M: f64 = 9.00;
pub const GEMINI_3_1_FLASH_LITE_INPUT_PER_M: f64 = 0.25;
pub const GEMINI_3_1_FLASH_LITE_OUTPUT_PER_M: f64 = 1.50;
pub const GEMINI_3_PRO_INPUT_PER_M: f64 = 2.00;
pub const GEMINI_3_PRO_OUTPUT_PER_M: f64 = 12.00;
pub const GEMINI_3_1_PRO_INPUT_PER_M: f64 = 2.00;
pub const GEMINI_3_1_PRO_OUTPUT_PER_M: f64 = 12.00;

// Fallback list prices used when a model is unknown (USD / 1M tokens)
pub const FALLBACK_OPENAI_INPUT_PER_M: f64 = 2.50;
pub const FALLBACK_OPENAI_OUTPUT_PER_M: f64 = 10.00;
pub const FALLBACK_ANTHROPIC_INPUT_PER_M: f64 = 3.00;
pub const FALLBACK_ANTHROPIC_OUTPUT_PER_M: f64 = 15.00;
pub const FALLBACK_GEMINI_INPUT_PER_M: f64 = 0.30;
pub const FALLBACK_GEMINI_OUTPUT_PER_M: f64 = 2.50;
pub const FALLBACK_UNKNOWN_PROVIDER_INPUT_PER_M: f64 = 3.00;
pub const FALLBACK_UNKNOWN_PROVIDER_OUTPUT_PER_M: f64 = 15.00;

// ── Billing period discount ───────────────────────────────────────────────────

/// Fraction subtracted from the subscription cost for annual billing (20% off).
/// Matches the standard used by GitHub, Vercel, Railway, and Linear.
pub const ANNUAL_BILLING_DISCOUNT: f64 = 0.20;

// ── Tier volume discounts (applied to compute, storage, and LLM) ─────────────

// TODO: double check the prices with the factors and make sure they are multiplied by VOLUME_DISCOUNT
pub const TIER1_VOLUME_DISCOUNT: f64 = 1.0;
pub const TIER2_VOLUME_DISCOUNT: f64 = 1.0;
pub const TIER3_VOLUME_DISCOUNT: f64 = 0.85;
pub const TIER4_VOLUME_DISCOUNT: f64 = 0.70;

// ── Compute & storage billing rates ──────────────────────────────────────────

/// USD per GB·ms of function execution time.
pub const TIER1_COMPUTE_PER_GB_MS: f64 = 8.0e-9;
pub const TIER2_COMPUTE_PER_GB_MS: f64 = 8.0e-9;
pub const TIER3_COMPUTE_PER_GB_MS: f64 = 8.0e-9;
pub const TIER4_COMPUTE_PER_GB_MS: f64 = 8.0e-9;

/// USD per GB·ms of actively-used (hot) storage.
pub const TIER1_STORAGE_ACTIVE_PER_GB_MS: f64 = 269.0e-11;
pub const TIER2_STORAGE_ACTIVE_PER_GB_MS: f64 = 269.0e-11;
pub const TIER3_STORAGE_ACTIVE_PER_GB_MS: f64 = 269.0e-11;
pub const TIER4_STORAGE_ACTIVE_PER_GB_MS: f64 = 269.0e-11;

/// USD per GB·ms of retained (cold) storage.
pub const TIER1_STORAGE_RETAINED_PER_GB_MS: f64 = 3.8e-11;
pub const TIER2_STORAGE_RETAINED_PER_GB_MS: f64 = 3.8e-11;
pub const TIER3_STORAGE_RETAINED_PER_GB_MS: f64 = 3.8e-11;
pub const TIER4_STORAGE_RETAINED_PER_GB_MS: f64 = 3.8e-11;

// ── Platform resource rates ───────────────────────────────────────────────────

/// Monthly subscription fee (USD). Converted to per-hour internally.
pub const TIER1_SUBSCRIPTION_MONTHLY_USD: f64 = 0.0;
pub const TIER2_SUBSCRIPTION_MONTHLY_USD: f64 = 0.0;
pub const TIER3_SUBSCRIPTION_MONTHLY_USD: f64 = 0.0;
pub const TIER4_SUBSCRIPTION_MONTHLY_USD: f64 = 0.0;

/// USD per project per hour.
pub const TIER1_PROJECT_PER_HOUR: f64 = 0.0;
pub const TIER2_PROJECT_PER_HOUR: f64 = 0.0;
pub const TIER3_PROJECT_PER_HOUR: f64 = 0.0;
pub const TIER4_PROJECT_PER_HOUR: f64 = 0.0;

/// USD per deployed unit (entity) per hour.
pub const TIER1_UNIT_PER_HOUR: f64 = 0.0;
pub const TIER2_UNIT_PER_HOUR: f64 = 0.0;
pub const TIER3_UNIT_PER_HOUR: f64 = 0.0;
pub const TIER4_UNIT_PER_HOUR: f64 = 0.0;

/// USD per stage runtime per hour.
pub const TIER1_STAGE_RUNTIME_PER_HOUR: f64 = 0.0;
pub const TIER2_STAGE_RUNTIME_PER_HOUR: f64 = 0.0;
pub const TIER3_STAGE_RUNTIME_PER_HOUR: f64 = 0.0;
pub const TIER4_STAGE_RUNTIME_PER_HOUR: f64 = 0.0;

// TODO: bring these to the game!
pub const TIER1_PROJECTS_COUNT_LIMIT: u32 = 2;
pub const TIER2_PROJECTS_COUNT_LIMIT: u32 = 3;
pub const TIER3_PROJECTS_COUNT_LIMIT: u32 = 10;
pub const TIER4_PROJECTS_COUNT_LIMIT: u32 = 40;

// TODO: bring these to the game!
pub const TIER1_STAGE_COUNT_LIMIT: u32 = 1;
pub const TIER2_STAGE_COUNT_LIMIT: u32 = 1;
pub const TIER3_STAGE_COUNT_LIMIT: u32 = 2;
pub const TIER4_STAGE_COUNT_LIMIT: u32 = 5;