// Rules specific to API units (`unit_type == "api"`).

// ── LLM hints (always compiled — no OXC dependency) ──────────────────────────────────────────

/// LLM-facing rule hints for API units.
///
/// Each entry is `(rule_name, hint)`.  These are used to build the combined rules prompt that
/// is injected into the developer agent's system prompt so the LLM knows the rules upfront.
pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "`handleRequest` must be exported with the correct signature",
            "Always export a function named `handleRequest` with exactly two parameters typed \
             `IncomingMessage` and `ServerResponse` (imported from `node:http`): \
             `import type { IncomingMessage, ServerResponse } from \"node:http\"; \
             export function handleRequest(req: IncomingMessage, res: ServerResponse) { … }`.",
        ),
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
            "Do not add any exports beyond `handleRequest` (the required function), \
             `RequestBody`, `ResponseBody`, and `QueryParams` (the required type aliases). \
             Forbidden: extra type alias exports, other value exports (`export const`, `export class`), \
             re-exports via `export { … }`, default exports (`export default`), \
             and wildcard re-exports (`export * from '…'`).",
        ),
    ]
}

// ── Linting rules (require the OXC parser) ────────────────────────────────────────────────────

#[cfg(feature = "unit_technical_reviewer")]
use crate::services::unit_technical_reviewer::rule_checker::{
    Declaration, Rule, RuleInput, Statement, export_all_source, get_exported_function_param_count,
    get_exported_function_param_type_name, get_exported_type, has_exported_function,
    has_exported_type, has_import_from, is_never, module_export_name_str,
};

#[cfg(feature = "unit_technical_reviewer")]
pub fn rules() -> Vec<Rule> {
    vec![
        handle_request_exported_correctly,
        request_body_exported,
        response_body_exported_and_not_never,
        query_params_exported,
        no_other_exports,
    ]
}

// ── Rules ─────────────────────────────────────────────────────────────────────────────────────

/// `export function handleRequest(req: IncomingMessage, res: ServerResponse)` must be present.
///
/// Every API unit is the entry point of an HTTP handler.  The runtime will call `handleRequest`
/// with Node.js `IncomingMessage` and `ServerResponse` objects.  Both parameter types must be
/// annotated with the exact names imported from `node:http` (via `import type { … }`).
///
/// Specifically this rule checks:
///   1. An exported function named `handleRequest` exists.
///   2. It has exactly two parameters.
///   3. The first parameter is typed `IncomingMessage`.
///   4. The second parameter is typed `ServerResponse`.
///   5. Both `IncomingMessage` and `ServerResponse` are imported from `node:http`.
#[cfg(feature = "unit_technical_reviewer")]
fn handle_request_exported_correctly(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };

    let mut errors = Vec::new();

    if !has_exported_function(program, "handleRequest") {
        errors.push(
            "API unit must export a function named `handleRequest`; \
             e.g. `export function handleRequest(req: IncomingMessage, res: ServerResponse) { … }`"
                .to_string(),
        );
        // Cannot check signature if the function is absent.
        return errors;
    }

    // Verify parameter count.
    let param_count = get_exported_function_param_count(program, "handleRequest").unwrap_or(0);
    if param_count != 2 {
        errors.push(format!(
            "`handleRequest` must have exactly 2 parameters \
             (`req: IncomingMessage, res: ServerResponse`), found {param_count}"
        ));
    }

    // Verify first parameter type is `IncomingMessage`.
    match get_exported_function_param_type_name(program, "handleRequest", 0) {
        Some("IncomingMessage") => {}
        Some(other) => errors.push(format!(
            "`handleRequest` first parameter must be typed `IncomingMessage`, found `{other}`"
        )),
        None => errors.push(
            "`handleRequest` first parameter must have a type annotation of `IncomingMessage`"
                .to_string(),
        ),
    }

    // Verify second parameter type is `ServerResponse`.
    match get_exported_function_param_type_name(program, "handleRequest", 1) {
        Some("ServerResponse") => {}
        Some(other) => errors.push(format!(
            "`handleRequest` second parameter must be typed `ServerResponse`, found `{other}`"
        )),
        None => errors.push(
            "`handleRequest` second parameter must have a type annotation of `ServerResponse`"
                .to_string(),
        ),
    }

    // Verify both types are imported from `node:http`.
    if !has_import_from(program, "node:http") {
        errors.push(
            "`IncomingMessage` and `ServerResponse` must be imported from `node:http`: \
             `import type { IncomingMessage, ServerResponse } from \"node:http\"`"
                .to_string(),
        );
    }

    errors
}

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

/// Forbidden: any export other than the three required type aliases and `handleRequest`.
///
/// Permitted exports:
///   - `export function handleRequest(…)` — the required HTTP handler
///   - `export type RequestBody`, `export type ResponseBody`, `export type QueryParams`
///
/// Everything else is rejected: other type aliases, other value exports, re-exports via
/// specifiers, default exports, and wildcard `export * from '…'`.
#[cfg(feature = "unit_technical_reviewer")]
fn no_other_exports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    const ALLOWED_TYPES: &[&str] = &["RequestBody", "ResponseBody", "QueryParams"];
    const ALLOWED_FN: &str = "handleRequest";
    let mut errors = Vec::new();

    for stmt in &program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::TSTypeAliasDeclaration(alias) => {
                            let name = alias.id.name.as_str();
                            if !ALLOWED_TYPES.contains(&name) {
                                errors.push(format!(
                                    "API unit must not export the type `{name}`; \
                                     only `RequestBody`, `ResponseBody`, and `QueryParams` are allowed"
                                ));
                            }
                        }
                        Declaration::FunctionDeclaration(func) => {
                            let fn_name = func
                                .id
                                .as_ref()
                                .map_or("<anonymous>", |id| id.name.as_str());
                            if fn_name != ALLOWED_FN {
                                errors.push(format!(
                                    "API unit must not export function `{fn_name}`; \
                                     the only allowed function export is `handleRequest`"
                                ));
                            }
                        }
                        _ => errors.push(
                            "API unit must not have value exports other than `handleRequest` \
                             (`export const`, `export class`, etc. are forbidden)"
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
                         found: {}. Only direct declarations are allowed",
                        names.join(", ")
                    ));
                }
            }

            Statement::ExportDefaultDeclaration(_) => errors
                .push("API unit must not have a default export (`export default …`)".to_string()),

            Statement::ExportAllDeclaration(e) => errors.push(format!(
                "API unit must not use `export * from '{}'`; \
                 only direct declarations are allowed",
                export_all_source(e)
            )),

            _ => {}
        }
    }

    errors
}
