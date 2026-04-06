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

use super::RuleInput;
use super::Rule;

pub(super) fn rules() -> Vec<Rule> {
    vec![
        // Example (uncomment and implement when needed):
        // no_noop_edit,
    ]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

// fn no_noop_edit(input: &RuleInput) -> Vec<String> {
//     // Compare input.program and input.old_program to detect unchanged code.
//     vec![]
// }
