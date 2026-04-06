// Rule-based linting for unit source code.
//
// Architecture:
//   1. `run_rule_check` parses `code` and `old_code` with OXC, then runs:
//        a. Generic rules — applied to every unit type and every operation.
//        b. Unit-type-specific rules — selected by `unit_type` slug (e.g. "api").
//      Both sets receive the full `RuleInput` context, so individual rules can choose to
//      skip based on `operation` (Create / Modify / Remove).
//   2. All rules execute independently — no short-circuit; every violation is collected
//      and returned so the LLM receives a complete picture of what to fix.
//   3. Pure utility functions are exposed for composing new rules from primitives.

// ── Public types ──────────────────────────────────────────────────────────────────────────────

/// Which LLM tool triggered the rule check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitOperation {
    Create,
    Modify,
    Remove,
}

// ── Real implementation (requires `unit_technical_reviewer` feature) ──────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
use oxc_allocator::Allocator;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_ast::ast::{
    Declaration, ExportAllDeclaration, ExportNamedDeclaration, ModuleExportName, Program,
    Statement, TSType,
};
#[cfg(feature = "unit_technical_reviewer")]
use oxc_parser::Parser;
#[cfg(feature = "unit_technical_reviewer")]
use oxc_span::SourceType;

// ── Public entry point ────────────────────────────────────────────────────────────────────────

/// Runs all applicable rules for the given operation and unit type.
///
/// - `operation`  — whether the LLM is creating, modifying, or removing the unit.
/// - `unit_type`  — unit type slug, e.g. `"api"`, `"action"`.
/// - `code`       — new source code (empty `""` for `Remove` operations).
/// - `old_code`   — previous source code (`None` for `Create`, `Some(&str)` for Modify/Remove).
///
/// Returns all rule violations as human-readable strings; an empty vec means everything passed.
#[cfg(feature = "unit_technical_reviewer")]
pub fn run_rule_check(
    operation: UnitOperation,
    unit_type: &str,
    code: &str,
    old_code: Option<&str>,
) -> Vec<String> {
    let allocator = Allocator::default();
    let old_allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);

    // Parse new code — skip for Remove (there is no new code).
    let new_ret = if !code.is_empty() {
        let ret = Parser::new(&allocator, code, source_type).parse();
        if !ret.errors.is_empty() {
            // The TypeScript compiler check should have caught parse errors first; surface
            // them here as a safety net.
            return ret.errors.iter().map(|e| format!("Parse error: {e}")).collect();
        }
        Some(ret)
    } else {
        None
    };

    // Parse old code if provided.
    let old_ret = old_code
        .filter(|s| !s.is_empty())
        .map(|s| Parser::new(&old_allocator, s, source_type).parse());

    let input = RuleInput {
        operation,
        unit_type,
        program: new_ret.as_ref().map(|r| &r.program),
        old_program: old_ret.as_ref().map(|r| &r.program),
    };

    let mut errors: Vec<String> = Vec::new();

    for rule in generic_rules() {
        errors.extend(rule(&input));
    }

    for rule in rules_for_unit_type(unit_type) {
        errors.extend(rule(&input));
    }

    errors
}

/// Stub when the feature is disabled (e.g. the main web-server binary).
#[cfg(not(feature = "unit_technical_reviewer"))]
pub fn run_rule_check(
    _operation: UnitOperation,
    _unit_type: &str,
    _code: &str,
    _old_code: Option<&str>,
) -> Vec<String> {
    vec![]
}

// ── Internal context & rule types ─────────────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
struct RuleInput<'a> {
    operation: UnitOperation,
    unit_type: &'a str,
    /// Parsed new code. `Some` for Create and Modify; `None` for Remove.
    program: Option<&'a Program<'a>>,
    /// Parsed old code. `Some` for Modify and Remove; `None` for Create.
    old_program: Option<&'a Program<'a>>,
}

#[cfg(feature = "unit_technical_reviewer")]
type Rule = for<'a> fn(&RuleInput<'a>) -> Vec<String>;

// ── Rule sets ─────────────────────────────────────────────────────────────────────────────────

/// Rules applied to every unit type and every operation before type-specific rules.
#[cfg(feature = "unit_technical_reviewer")]
fn generic_rules() -> Vec<Rule> {
    vec![
        // Add cross-cutting rules here, e.g.:
        //   generic_rule_no_console_log,
        //   generic_rule_no_process_exit,
    ]
}

/// Selects the rule set for a given unit type slug.
/// Each rule receives the full `RuleInput` and decides internally whether it applies to the
/// current `operation`.
#[cfg(feature = "unit_technical_reviewer")]
fn rules_for_unit_type(unit_type: &str) -> Vec<Rule> {
    match unit_type {
        "api" => api_unit_rules(),
        // Add other unit types here as their rules are defined:
        //   "action"     => action_unit_rules(),
        //   "command"    => command_unit_rules(),
        _ => vec![],
    }
}

// ── API unit rules ────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
fn api_unit_rules() -> Vec<Rule> {
    vec![
        // Create / Modify: structural shape of the new code.
        api_rule_request_body_exported,
        api_rule_response_body_exported_and_not_never,
        api_rule_query_params_exported,
        api_rule_no_other_exports,
        // Modify-only rules (comparing old ↔ new code) go here, e.g.:
        //   api_rule_response_body_not_narrowed,
        // Remove-only rules go here, e.g.:
        //   api_rule_no_dependents_before_remove,
    ]
}

// — Create / Modify rules (skip for Remove) —

/// `export type RequestBody` must be present (may be `never`).
#[cfg(feature = "unit_technical_reviewer")]
fn api_rule_request_body_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else { return vec![] };
    if has_exported_type(program, "RequestBody") {
        vec![]
    } else {
        vec![
            "API unit must export `export type RequestBody` \
             (the type can be `never` if there is no request body, but the export must be present)"
                .to_string(),
        ]
    }
}

/// `export type ResponseBody` must be present AND must not be `never`.
#[cfg(feature = "unit_technical_reviewer")]
fn api_rule_response_body_exported_and_not_never(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else { return vec![] };
    match get_exported_type(program, "ResponseBody") {
        None => vec![
            "API unit must export `export type ResponseBody = { ... }` \
             (it cannot be `never` — every API must describe its response shape)"
                .to_string(),
        ],
        Some(ty) if is_never(ty) => vec![
            "API unit `ResponseBody` must not be `never`; \
             define the actual response object type (e.g. `export type ResponseBody = { id: string }`)"
                .to_string(),
        ],
        _ => vec![],
    }
}

/// `export type QueryParams` must be present (may be `never`).
#[cfg(feature = "unit_technical_reviewer")]
fn api_rule_query_params_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else { return vec![] };
    if has_exported_type(program, "QueryParams") {
        vec![]
    } else {
        vec![
            "API unit must export `export type QueryParams` \
             (the type can be `never` if there are no query parameters, but the export must be present)"
                .to_string(),
        ]
    }
}

/// Only `RequestBody`, `ResponseBody`, and `QueryParams` type aliases may be exported.
/// Value exports, re-exports, default exports, and `export *` are all forbidden.
#[cfg(feature = "unit_technical_reviewer")]
fn api_rule_no_other_exports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else { return vec![] };
    const ALLOWED: &[&str] = &["RequestBody", "ResponseBody", "QueryParams"];
    let mut errors = Vec::new();

    for stmt in &program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::TSTypeAliasDeclaration(alias) => {
                            let name = alias.id.name.as_str();
                            if !ALLOWED.contains(&name) {
                                errors.push(format!(
                                    "API unit must not export the type `{name}`; \
                                     only `RequestBody`, `ResponseBody`, and `QueryParams` are allowed"
                                ));
                            }
                        }
                        _ => errors.push(
                            "API unit must not have value exports \
                             (`export const`, `export function`, `export class`, etc.); \
                             only `RequestBody`, `ResponseBody`, and `QueryParams` type exports are allowed"
                                .to_string(),
                        ),
                    }
                }

                if !export.specifiers.is_empty() {
                    let names: Vec<&str> = export
                        .specifiers
                        .iter()
                        .map(|s| module_export_name_str(&s.exported))
                        .collect();
                    errors.push(format!(
                        "API unit must not re-export names via `export {{ ... }}`; \
                         found: {}. Only direct type declarations are allowed",
                        names.join(", ")
                    ));
                }
            }

            Statement::ExportDefaultDeclaration(_) => errors.push(
                "API unit must not have a default export (`export default`); \
                 only named type exports are allowed"
                    .to_string(),
            ),

            Statement::ExportAllDeclaration(e) => errors.push(format!(
                "API unit must not use `export * from '{}'`; \
                 only named type exports are allowed",
                export_all_source(e)
            )),

            _ => {}
        }
    }

    errors
}

// ── Pure utility functions ────────────────────────────────────────────────────────────────────
//
// Building blocks for composing rules. Each function is pure: reads the program and returns
// a value — no side effects, no error accumulation.

/// Returns the names of all top-level exported type aliases, in source order.
#[cfg(feature = "unit_technical_reviewer")]
pub fn get_exported_type_names<'a>(program: &'a Program) -> Vec<&'a str> {
    program
        .body
        .iter()
        .filter_map(|stmt| {
            if let Statement::ExportNamedDeclaration(export) = stmt {
                if let Some(Declaration::TSTypeAliasDeclaration(alias)) = &export.declaration {
                    return Some(alias.id.name.as_str());
                }
            }
            None
        })
        .collect()
}

/// Returns `true` if a type alias named `name` is exported at the top level.
#[cfg(feature = "unit_technical_reviewer")]
pub fn has_exported_type(program: &Program, name: &str) -> bool {
    get_exported_type_names(program).contains(&name)
}

/// Returns a reference to the `TSType` of the exported type alias named `name`, if it exists.
///
/// ```ignore
/// if let Some(ty) = get_exported_type(program, "ResponseBody") {
///     if is_never(ty) { /* ... */ }
/// }
/// ```
#[cfg(feature = "unit_technical_reviewer")]
pub fn get_exported_type<'a>(program: &'a Program<'a>, name: &str) -> Option<&'a TSType<'a>> {
    program.body.iter().find_map(|stmt| {
        if let Statement::ExportNamedDeclaration(export) = stmt {
            if let Some(Declaration::TSTypeAliasDeclaration(alias)) = &export.declaration {
                if alias.id.name.as_str() == name {
                    return Some(&alias.type_annotation);
                }
            }
        }
        None
    })
}

/// Returns `true` if the type is the `never` keyword.
#[cfg(feature = "unit_technical_reviewer")]
pub fn is_never(ty: &TSType) -> bool {
    matches!(ty, TSType::TSNeverKeyword(_))
}

/// Returns `true` if the type is an inline object literal (`{ field: Type; … }`).
#[cfg(feature = "unit_technical_reviewer")]
pub fn is_object_type(ty: &TSType) -> bool {
    matches!(ty, TSType::TSTypeLiteral(_))
}

/// Returns all top-level `ExportNamedDeclaration` nodes (type and value alike).
#[cfg(feature = "unit_technical_reviewer")]
pub fn get_export_named_decls<'a>(program: &'a Program<'a>) -> Vec<&'a ExportNamedDeclaration<'a>> {
    program
        .body
        .iter()
        .filter_map(|stmt| {
            if let Statement::ExportNamedDeclaration(e) = stmt {
                Some(e.as_ref())
            } else {
                None
            }
        })
        .collect()
}

/// Returns a description of every export that is NOT a type alias declaration.
///
/// Useful for writing "no value exports" rules:
/// ```ignore
/// let bad = get_non_type_alias_exports(program);
/// if !bad.is_empty() { /* fail */ }
/// ```
#[cfg(feature = "unit_technical_reviewer")]
pub fn get_non_type_alias_exports(program: &Program) -> Vec<String> {
    let mut result = Vec::new();
    for stmt in &program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    if !matches!(decl, Declaration::TSTypeAliasDeclaration(_)) {
                        result.push("non-type named export (const / function / class / …)".to_string());
                    }
                }
                for spec in &export.specifiers {
                    result.push(format!("re-export `{}`", module_export_name_str(&spec.exported)));
                }
            }
            Statement::ExportDefaultDeclaration(_) => result.push("default export".to_string()),
            Statement::ExportAllDeclaration(e) => {
                result.push(format!("export * from '{}'", export_all_source(e)));
            }
            _ => {}
        }
    }
    result
}

// ── Internal helpers ──────────────────────────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
fn module_export_name_str<'a>(name: &'a ModuleExportName<'a>) -> &'a str {
    match name {
        ModuleExportName::IdentifierName(id) => id.name.as_str(),
        ModuleExportName::IdentifierReference(id) => id.name.as_str(),
        ModuleExportName::StringLiteral(s) => s.value.as_str(),
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn export_all_source<'a>(e: &'a ExportAllDeclaration<'a>) -> &'a str {
    e.source.value.as_str()
}
