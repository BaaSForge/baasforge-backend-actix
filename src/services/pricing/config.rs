// ── LLM ──────────────────────────────────────────────────────────────────────

/// Markup applied on top of every provider list price.
pub const LLM_PLATFORM_MARGIN: f64 = 2.0;

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
