// Generic rules that only apply to Create operations.
//
// These rules are evaluated when the LLM calls `create_unit`.  They run AFTER `generic_rules`
// and BEFORE unit-type-specific rules.
//
// Good candidates for this file:
//   - "A newly created unit must not already use deprecated patterns."
//   - "A new unit must not import from units that do not exist yet."
//   - Code-level constraints that only make sense at creation time, not when modifying.
//
// Add new rules to the `rules()` vec below and implement them as private functions in this file.

// ── LLM hints (always compiled — no OXC dependency) ──────────────────────────────────────────

/// LLM-facing rule hints for Create operations.
///
/// Each entry is `(rule_name, hint)`.  These are used to build the combined rules prompt that
/// is injected into the developer agent's system prompt so the LLM knows the rules upfront.
pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        // Add hints here as new create-specific rules are introduced.
    ]
}

// ── Linting rules (require the OXC parser) ────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
use super::Rule;
#[cfg(feature = "unit_technical_reviewer")]
use super::RuleInput;

#[cfg(feature = "unit_technical_reviewer")]
pub(super) fn rules() -> Vec<Rule> {
    vec![
        // Example (uncomment and implement when needed):
        // no_deprecated_imports,
    ]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

// #[cfg(feature = "unit_technical_reviewer")]
// fn no_deprecated_imports(input: &RuleInput) -> Vec<String> {
//     let Some(_program) = input.program else { return vec![] };
//     vec![]
// }
