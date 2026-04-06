// Rule-based linting for unit source code.
//
// Entry point: `run_rule_check`.
// Each rule function receives a `RuleInput` context and returns a (possibly empty) list of
// human-readable error strings. All rules in a set are always run — no short-circuit — so the
// LLM gets a full list of violations in a single pass.
//
// Rule execution order:
//   1. Generic rules   (every unit type, every operation) — generic_rules.rs
//   2. Operation rules (every unit type, current operation only)
//       Create  → create_rules.rs
//       Modify  → modify_rules.rs
//       Remove  → remove_rules.rs
//   3. Unit-type rules (current unit type, every operation) — e.g. api_rules.rs

// ── Submodules ────────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
pub mod ast_utils;
#[cfg(feature = "unit_technical_reviewer")]
mod generic_rules;
#[cfg(feature = "unit_technical_reviewer")]
mod create_rules;
#[cfg(feature = "unit_technical_reviewer")]
mod modify_rules;
#[cfg(feature = "unit_technical_reviewer")]
mod remove_rules;
#[cfg(feature = "unit_technical_reviewer")]
mod api_rules;

// ── Re-export OXC utilities so callers can compose new rules without knowing the internal layout

#[cfg(feature = "unit_technical_reviewer")]
pub use ast_utils::{
    contains_word, get_export_named_decls, get_exported_type, get_exported_type_names,
    get_non_type_alias_exports, has_exported_type, is_never, is_object_type,
};

// ── Public types (always compiled) ───────────────────────────────────────────────────────────

/// Which LLM tool triggered the rule check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitOperation {
    Create,
    Modify,
    Remove,
}

// ── Internal types (visible to all submodules of this module) ─────────────────────────────────

/// Context passed to every rule function. A rule may inspect any combination of fields.
#[cfg(feature = "unit_technical_reviewer")]
struct RuleInput<'a> {
    pub operation: UnitOperation,
    pub unit_type: &'a str,
    /// Parsed new code.  `Some` for Create and Modify; `None` for Remove.
    pub program: Option<&'a oxc_ast::ast::Program<'a>>,
    /// Parsed old code.  `Some` for Modify and Remove; `None` for Create.
    pub old_program: Option<&'a oxc_ast::ast::Program<'a>>,
    /// Raw source text of the new code (`""` for Remove).
    /// Available for text-based checks (e.g. word-boundary searches) that are simpler to
    /// implement without a full AST walk.
    pub source: &'a str,
    /// Raw source text of the old code (`""` for Create).
    pub old_source: &'a str,
}

/// Every rule is a plain function: pure, no I/O, returns zero or more violation messages.
#[cfg(feature = "unit_technical_reviewer")]
type Rule = for<'a> fn(&RuleInput<'a>) -> Vec<String>;

// ── Public entry point ────────────────────────────────────────────────────────────────────────

/// Runs all applicable rules for the given operation and unit type.
///
/// - `operation`  — whether the LLM is creating, modifying, or removing the unit.
/// - `unit_type`  — unit type slug, e.g. `"api"`, `"action"`.
/// - `code`       — new source code (pass `""` for Remove).
/// - `old_code`   — previous source code (`None` for Create; `Some` for Modify/Remove).
///
/// Returns every rule violation as a human-readable string.
/// An empty vec means all rules passed.
#[cfg(feature = "unit_technical_reviewer")]
pub fn run_rule_check(
    operation: UnitOperation,
    unit_type: &str,
    code: &str,
    old_code: Option<&str>,
) -> Vec<String> {
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    let allocator = Allocator::default();
    let old_allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);

    // Parse new code (skip for Remove — there is none).
    let new_ret = if !code.is_empty() {
        let ret = Parser::new(&allocator, code, source_type).parse();
        if !ret.errors.is_empty() {
            // The TypeScript compiler check should have caught these first; surface them here as
            // a safety net.
            return ret.errors.iter().map(|e| format!("Parse error: {e}")).collect();
        }
        Some(ret)
    } else {
        None
    };

    // Parse old code when provided.
    let old_ret = old_code
        .filter(|s| !s.is_empty())
        .map(|s| Parser::new(&old_allocator, s, source_type).parse());

    let input = RuleInput {
        operation,
        unit_type,
        program: new_ret.as_ref().map(|r| &r.program),
        old_program: old_ret.as_ref().map(|r| &r.program),
        source: code,
        old_source: old_code.unwrap_or(""),
    };

    let mut errors: Vec<String> = Vec::new();

    // 1. Generic — all unit types, all operations.
    for rule in generic_rules::rules() {
        errors.extend(rule(&input));
    }

    // 2. Operation-specific generic rules.
    let op_rules: Vec<Rule> = match operation {
        UnitOperation::Create => create_rules::rules(),
        UnitOperation::Modify => modify_rules::rules(),
        UnitOperation::Remove => remove_rules::rules(),
    };
    for rule in op_rules {
        errors.extend(rule(&input));
    }

    // 3. Unit-type-specific rules.
    for rule in unit_type_rules(unit_type) {
        errors.extend(rule(&input));
    }

    errors
}

/// Stub when the `unit_technical_reviewer` feature is disabled (e.g. the main web-server binary).
#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn run_rule_check(
    _operation: UnitOperation,
    _unit_type: &str,
    _code: &str,
    _old_code: Option<&str>,
) -> Vec<String> {
    vec![]
}

// ── Unit-type dispatch ────────────────────────────────────────────────────────────────────────

/// Returns the rule set for the given unit-type slug.
/// Add new unit types here as their rule files are created.
#[cfg(feature = "unit_technical_reviewer")]
fn unit_type_rules(unit_type: &str) -> Vec<Rule> {
    match unit_type {
        "api" => api_rules::rules(),
        // "action"     => action_rules::rules(),
        // "command"    => command_rules::rules(),
        _ => vec![],
    }
}
