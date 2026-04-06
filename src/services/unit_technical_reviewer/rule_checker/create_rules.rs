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

use super::RuleInput;
use super::Rule;

pub(super) fn rules() -> Vec<Rule> {
    vec![
        // Example (uncomment and implement when needed):
        // no_deprecated_imports,
    ]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

// fn no_deprecated_imports(input: &RuleInput) -> Vec<String> {
//     let Some(_program) = input.program else { return vec![] };
//     vec![]
// }
