// Generic rules — applied to every unit type and every operation.
//
// Rules here express invariants that hold across the entire codebase regardless of unit type
// or operation.  Rules that are specific to a single unit type belong in that type's rule file
// (e.g. `api_rules.rs`).  Rules that only make sense for one operation belong in the matching
// operation rule file (`create_rules.rs`, `modify_rules.rs`, `remove_rules.rs`).
//
// Text-based rules (checking the raw source string) use `ast_utils::contains_word` which
// performs whole-identifier matching so it doesn't fire on `consoleHelper` or `processData`.
// AST-based rules use the parsed `input.program` (new code) or `input.old_program` (old code).

use super::ast_utils;
use super::Rule;
use super::RuleInput;

pub(super) fn rules() -> Vec<Rule> {
    vec![no_console, no_process]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

/// Units must not reference the `console` global.
///
/// Server-side units run inside a controlled runtime; structured logging must be used instead of
/// ad-hoc `console.log` / `console.error` calls.  Leaving console statements in production code
/// leaks internal information and bypasses log aggregation.
fn no_console(input: &RuleInput) -> Vec<String> {
    if input.source.is_empty() {
        return vec![];
    }
    if ast_utils::contains_word(input.source, "console") {
        vec![
            "Do not use `console` in units (e.g. `console.log`, `console.error`). \
             Use the project's structured logger instead."
                .to_string(),
        ]
    } else {
        vec![]
    }
}

/// Units must not reference the `process` global.
///
/// Directly accessing `process.env`, `process.exit`, `process.argv`, etc. couples a unit to
/// Node.js runtime internals and makes the code harder to test and port.  Environment
/// configuration must flow in through the unit's explicit inputs.
fn no_process(input: &RuleInput) -> Vec<String> {
    if input.source.is_empty() {
        return vec![];
    }
    if ast_utils::contains_word(input.source, "process") {
        vec![
            "Do not access `process` directly in units (e.g. `process.env`, `process.exit`). \
             Receive configuration through the unit's explicit inputs."
                .to_string(),
        ]
    } else {
        vec![]
    }
}
