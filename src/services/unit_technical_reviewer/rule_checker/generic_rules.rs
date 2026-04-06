// Generic rules — applied to every unit type and every operation.
//
// Rules here express invariants that hold across the entire codebase regardless of unit type
// or operation.  Rules that are specific to a single unit type belong in that type's rule file
// (e.g. `units/api_rules.rs`).  Rules that only make sense for one operation belong in the matching
// operation rule file (`create_rules.rs`, `modify_rules.rs`, `remove_rules.rs`).
//
// All rules here use `ast_utils::has_identifier_reference` rather than raw-string searches so
// that comments, string literals, and template literal text cannot produce false positives.
// AST-based rules use the parsed `input.program` (new code) or `input.old_program` (old code).

// ── LLM hints (always compiled — no OXC dependency) ──────────────────────────────────────────

/// LLM-facing rule hints for generic rules (all unit types, all operations).
///
/// Each entry is `(rule_name, hint)`.  These are used to build the combined rules prompt that
/// is injected into the developer agent's system prompt so the LLM knows the rules upfront.
pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "No `console` usage",
            "Never use `console` in any unit (e.g. `console.log`, `console.error`, `console.warn`).",
        ),
        (
            "No `process` access",
            "Never access `process` directly in any unit \
             (e.g. `process.env`, `process.exit`, `process.argv`).",
        ),
    ]
}

// ── Linting rules (require the OXC parser) ────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
use super::ast_utils;
#[cfg(feature = "unit_technical_reviewer")]
use super::Rule;
#[cfg(feature = "unit_technical_reviewer")]
use super::RuleInput;

#[cfg(feature = "unit_technical_reviewer")]
pub(super) fn rules() -> Vec<Rule> {
    vec![no_console, no_process]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

/// Units must not reference the `console` global.
///
/// Server-side units run inside a controlled runtime; structured logging must be used instead of
/// ad-hoc `console.log` / `console.error` calls.  Leaving console statements in production code
/// leaks internal information and bypasses log aggregation.
///
/// Detection is AST-based: only real `console` identifier references in executable code are
/// flagged — occurrences inside comments or string literals are ignored.
#[cfg(feature = "unit_technical_reviewer")]
fn no_console(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if ast_utils::has_identifier_reference(program, "console") {
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
///
/// Detection is AST-based: only real `process` identifier references in executable code are
/// flagged — occurrences inside comments or string literals are ignored.
#[cfg(feature = "unit_technical_reviewer")]
fn no_process(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if ast_utils::has_identifier_reference(program, "process") {
        vec![
            "Do not access `process` directly in units (e.g. `process.env`, `process.exit`). \
             Receive configuration through the unit's explicit inputs."
                .to_string(),
        ]
    } else {
        vec![]
    }
}
