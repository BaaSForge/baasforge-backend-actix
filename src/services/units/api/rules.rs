// Rules specific to API units (`unit_type == "api"`).
//
// An API unit is the TypeScript contract between a client caller and a server handler.
// Its only job is to declare the three shapes that define a single HTTP endpoint:
//
//   export type RequestBody  = … | never   (what the client sends in the body)
//   export type ResponseBody = { … }       (what the server returns — must be a concrete type)
//   export type QueryParams  = … | never   (URL query parameters)
//
// Nothing else may be exported from an API unit.  Value exports (const, function, class),
// re-exports, default exports, and wildcard exports are all forbidden.
//
// Rules here receive the full `RuleInput` context.  All four rules below guard with
// `let Some(program) = input.program else { return vec![] };` so they are automatically
// skipped for Remove operations (which have no new code to check).

// ── LLM hints (always compiled — no OXC dependency) ──────────────────────────────────────────

/// LLM-facing rule hints for API units.
///
/// Each entry is `(rule_name, hint)`.  These are used to build the combined rules prompt that
/// is injected into the developer agent's system prompt so the LLM knows the rules upfront.
pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "`RequestBody` must be exported",
            "Always include `export type RequestBody = …` in an API unit. \
             If the endpoint takes no request body, use `export type RequestBody = never`. \
             The export must always be present.",
        ),
        (
            "`ResponseBody` must be exported and must not be `never`",
            "Always include `export type ResponseBody = { … }` with a concrete object type. \
             `never` is forbidden for `ResponseBody` — every API must describe its actual response shape.",
        ),
        (
            "`QueryParams` must be exported",
            "Always include `export type QueryParams = …` in an API unit. \
             If the endpoint takes no query parameters, use `export type QueryParams = never`. \
             The export must always be present.",
        ),
        (
            "No other exports allowed",
            "Only `RequestBody`, `ResponseBody`, and `QueryParams` may be exported from an API unit. \
             Do not add any other type alias exports, value exports (`export const`, `export function`, \
             `export class`), re-exports via `export { … }`, default exports (`export default`), \
             or wildcard re-exports (`export * from '…'`).",
        ),
    ]
}

// ── Linting rules (require the OXC parser) ────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
use crate::services::unit_technical_reviewer::rule_checker::{
    export_all_source, get_exported_type, has_exported_type, is_never, module_export_name_str,
    Declaration, Rule, RuleInput, Statement,
};

#[cfg(feature = "unit_technical_reviewer")]
pub fn rules() -> Vec<Rule> {
    vec![
        request_body_exported,
        response_body_exported_and_not_never,
        query_params_exported,
        no_other_exports,
    ]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

/// `export type RequestBody` must be present.  The value may be `never` (no body).
#[cfg(feature = "unit_technical_reviewer")]
fn request_body_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if has_exported_type(program, "RequestBody") {
        vec![]
    } else {
        vec![
            "API unit must export `export type RequestBody` \
             (use `never` if there is no request body, but the export must be present)"
                .to_string(),
        ]
    }
}

/// `export type ResponseBody` must be present AND must not be `never`.
/// Every API must describe its concrete response shape.
#[cfg(feature = "unit_technical_reviewer")]
fn response_body_exported_and_not_never(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    match get_exported_type(program, "ResponseBody") {
        None => vec![
            "API unit must export `export type ResponseBody = { … }` \
             (`never` is not allowed — define the actual response object shape)"
                .to_string(),
        ],
        Some(ty) if is_never(ty) => vec![
            "API unit `ResponseBody` must not be `never`; \
             provide the response object type, e.g. `export type ResponseBody = { id: string }`"
                .to_string(),
        ],
        _ => vec![],
    }
}

/// `export type QueryParams` must be present.  The value may be `never` (no params).
#[cfg(feature = "unit_technical_reviewer")]
fn query_params_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if has_exported_type(program, "QueryParams") {
        vec![]
    } else {
        vec![
            "API unit must export `export type QueryParams` \
             (use `never` if there are no query parameters, but the export must be present)"
                .to_string(),
        ]
    }
}

/// Only `RequestBody`, `ResponseBody`, and `QueryParams` type aliases may be exported.
///
/// Forbidden: any other type alias, value exports (const / function / class), re-exports via
/// specifiers, default exports, and wildcard `export * from '…'`.
#[cfg(feature = "unit_technical_reviewer")]
fn no_other_exports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
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
                        "API unit must not re-export names via `export {{ … }}`; \
                         found: {}. Only direct type alias declarations are allowed",
                        names.join(", ")
                    ));
                }
            }

            Statement::ExportDefaultDeclaration(_) => errors.push(
                "API unit must not have a default export (`export default …`); \
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
