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
//   3. Unit-type rules (current unit type, every operation) — units/<type>_rules.rs
//
// Each rule file also exposes `hint_entries()` (no feature gate) which feeds `rules_prompt()`,
// the single source of truth for what rules the LLM developer agent must follow.

// ── Submodules ────────────────────────────────────────────────────────────────────────────────
//
// The *module* declarations below are not feature-gated so that `hint_entries()` (which has no
// OXC dependency) compiles for every binary including the chat_worker.  The OXC-dependent code
// inside each file is individually gated with `#[cfg(feature = "unit_technical_reviewer")]`.

mod generic_rules;
mod create_rules;
mod modify_rules;
mod remove_rules;

#[cfg(feature = "unit_technical_reviewer")]
pub mod ast_utils;

// ── Re-export OXC utilities so callers can compose new rules without knowing the internal layout

#[cfg(feature = "unit_technical_reviewer")]
pub use ast_utils::{
    contains_word, export_all_source, get_export_named_decls, get_export_default_call_name,
    get_exported_type, get_exported_type_names, get_import_declarations, get_imports_from_source,
    get_named_imports_from_source, get_node_module_import_sources, get_non_type_alias_exports,
    has_default_import_from, has_export_default, has_exported_function, has_exported_type,
    has_identifier_reference, has_import_from, has_namespace_import_from,
    get_exported_function_names, get_exported_function_param_count,
    get_exported_function_param_type_name, is_never, is_object_type, module_export_name_str,
    ts_type_reference_name,
};

#[cfg(feature = "unit_technical_reviewer")]
pub use oxc_ast::ast::{Declaration, Statement};

// ── TypeScript → JavaScript compiler ─────────────────────────────────────────────────────────

/// Transpiles TypeScript source code to JavaScript by stripping type annotations.
///
/// Uses OXC's transformer (type-strip only — no down-level emit). The output preserves
/// ES module syntax and is suitable for runtime execution in a modern JS environment.
///
/// Returns `Ok(js_code)` on success, or `Err(message)` if parsing fails.
#[cfg(feature = "unit_technical_reviewer")]
pub fn compile_ts_to_js(ts_source: &str) -> Result<String, String> {
    use oxc_allocator::Allocator;
    use oxc_codegen::Codegen;
    use oxc_parser::Parser;
    use oxc_semantic::SemanticBuilder;
    use oxc_span::SourceType;
    use oxc_transformer::{TransformOptions, Transformer};
    use std::path::Path;

    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);

    let ret = Parser::new(&allocator, ts_source, source_type).parse();
    if !ret.errors.is_empty() {
        let msg = ret
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("Parse error: {msg}"));
    }

    let mut program = ret.program;

    let scoping = SemanticBuilder::new()
        .with_excess_capacity(2.0)
        .build(&program)
        .semantic
        .into_scoping();

    let transform_ret = Transformer::new(&allocator, Path::new("module.ts"), &TransformOptions::default())
        .build_with_scoping(scoping, &mut program);

    if !transform_ret.errors.is_empty() {
        let msg = transform_ret
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("Transform error: {msg}"));
    }

    Ok(Codegen::new().build(&program).code)
}

/// Stub when the `unit_technical_reviewer` feature is disabled.
#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn compile_ts_to_js(_ts_source: &str) -> Result<String, String> {
    Ok(String::new())
}

// ── Dependency extractor ──────────────────────────────────────────────────────────────────────

/// Parses `ts_source` and returns a deduplicated, sorted, comma-separated list of unit paths
/// imported via `@units/<path>` specifiers.
///
/// A unit path has the form `{context}.{module}.{unit_type}.{unit_slug}` (no revision).
/// Any import declaration whose module specifier starts with `@units/` is treated as a unit
/// dependency; the `@units/` prefix is stripped to produce the path.
///
/// Only actual `import` AST nodes are visited — occurrences in comments or string literals are
/// ignored.  Returns `None` when the source cannot be parsed or has no `@units/` imports.
#[cfg(feature = "unit_technical_reviewer")]
pub fn extract_unit_dependencies(ts_source: &str) -> Option<String> {
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;
    use std::collections::BTreeSet;

    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);

    let ret = Parser::new(&allocator, ts_source, source_type).parse();
    if !ret.errors.is_empty() {
        return None;
    }

    let mut deps: BTreeSet<String> = BTreeSet::new();

    for stmt in &ret.program.body {
        if let oxc_ast::ast::Statement::ImportDeclaration(import_decl) = stmt {
            let source = import_decl.source.value.as_str();
            if let Some(path) = source.strip_prefix("@units/") {
                if !path.is_empty() {
                    deps.insert(path.to_string());
                }
            }
        }
    }

    if deps.is_empty() {
        None
    } else {
        Some(deps.into_iter().collect::<Vec<_>>().join(","))
    }
}

/// Stub when the `unit_technical_reviewer` feature is disabled.
#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn extract_unit_dependencies(_ts_source: &str) -> Option<String> {
    None
}

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
pub(crate) struct RuleInput<'a> {
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
pub(crate) type Rule = for<'a> fn(&RuleInput<'a>) -> Vec<String>;

// ── Rules prompt (always compiled) ───────────────────────────────────────────────────────────

/// Returns a Markdown string listing all enforced rules grouped by category.
///
/// This is the **single source of truth** for the rules the LLM must follow.  The same rules
/// that are checked by `run_rule_check` are described here as human-readable hints.  Call this
/// from the developer agent's system prompt so the LLM knows the constraints upfront.
pub fn rules_prompt() -> String {
    let mut out = String::new();

    // 1. Generic rules
    out.push_str("## 1. Generic Rules\n\n");
    append_hints(&mut out, generic_rules::hint_entries());

    // 2. Create unit rules
    out.push_str("## 2. Create Unit Rules\n\n");
    append_hints(&mut out, create_rules::hint_entries());

    // 3. Modify unit rules
    out.push_str("## 3. Modify Unit Rules\n\n");
    append_hints(&mut out, modify_rules::hint_entries());

    // 4. Remove unit rules
    out.push_str("## 4. Remove Unit Rules\n\n");
    append_hints(&mut out, remove_rules::hint_entries());

    // 5. Unit-specific rules
    out.push_str("## 5. Unit-Specific Rules\n\n");

    out.push_str("### API Units\n\n");
    append_hints(&mut out, crate::services::units::api::rules::hint_entries());

    out.push_str("### Workflow Units\n\n");
    append_hints(&mut out, crate::services::units::workflow::rules::hint_entries());

    out.push_str("### Actor Units\n\n");
    append_hints(&mut out, crate::services::units::actor::rules::hint_entries());

    out.push_str("### Queue Worker Units\n\n");
    append_hints(&mut out, crate::services::units::queue_worker::rules::hint_entries());

    out
}

fn append_hints(out: &mut String, entries: Vec<(&'static str, &'static str)>) {
    if entries.is_empty() {
        out.push_str("*(No additional rules for this group.)*\n\n");
    } else {
        for (name, hint) in entries {
            out.push_str(&format!("- **{name}**: {hint}\n"));
        }
        out.push('\n');
    }
}

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
        "api" => crate::services::units::api::rules::rules(),
        "workflow" => crate::services::units::workflow::rules::rules(),
        "queue_worker" => crate::services::units::queue_worker::rules::rules(),
        "actor" => crate::services::units::actor::rules::rules(),
        _ => vec![],
    }
}
