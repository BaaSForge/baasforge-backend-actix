// Generic rules that only apply to Modify operations.
//
// These rules are evaluated when the LLM calls `modify_unit`.  They run AFTER `generic_rules`
// and BEFORE unit-type-specific rules.
//
// Both the new code (`input.program`) and the previous code (`input.old_program`) are available,
// so these rules are the right place to enforce constraints that span old ↔ new:
//   - "A modification must not remove a public type that other units may depend on."
//   - "The exported interface must remain backwards-compatible."
//   - "A revision must actually change something (no no-op edits)."
//
// Add new rules to the `rules()` vec below and implement them as private functions in this file.

// ── LLM hints (always compiled — no OXC dependency) ──────────────────────────────────────────

/// LLM-facing rule hints for Modify operations.
///
/// Each entry is `(rule_name, hint)`.  These are used to build the combined rules prompt that
/// is injected into the developer agent's system prompt so the LLM knows the rules upfront.
pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        // Add hints here as new modify-specific rules are introduced.
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
        // no_noop_edit,
    ]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

// #[cfg(feature = "unit_technical_reviewer")]
// fn no_noop_edit(input: &RuleInput) -> Vec<String> {
//     // Compare input.program and input.old_program to detect unchanged code.
//     vec![]
// }
