// Generic rules that only apply to Remove operations.
//
// These rules are evaluated when the LLM calls `remove_unit`.  They run AFTER `generic_rules`
// and BEFORE unit-type-specific rules.
//
// Only `input.old_program` is populated here (`input.program` is `None` because there is no new
// code).  Use `input.old_program` to inspect the unit that is about to be deleted.
//
// Good candidates for this file:
//   - "A unit flagged as `@pinned` must not be removed without explicit confirmation."
//   - "A unit that exports types referenced in other units cannot be silently deleted."
//   - Static-analysis guards that prevent accidental removal of load-bearing code.
//
// Add new rules to the `rules()` vec below and implement them as private functions in this file.

// ── LLM hints (always compiled — no OXC dependency) ──────────────────────────────────────────

/// LLM-facing rule hints for Remove operations.
///
/// Each entry is `(rule_name, hint)`.  These are used to build the combined rules prompt that
/// is injected into the developer agent's system prompt so the LLM knows the rules upfront.
pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        // Add hints here as new remove-specific rules are introduced.
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
        // no_remove_pinned,
    ]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

// #[cfg(feature = "unit_technical_reviewer")]
// fn no_remove_pinned(input: &RuleInput) -> Vec<String> {
//     let Some(_old_program) = input.old_program else { return vec![] };
//     // Check for a `@pinned` JSDoc annotation in the old source.
//     vec![]
// }
