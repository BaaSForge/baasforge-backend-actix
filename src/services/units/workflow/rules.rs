pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "`export default workflow(...)` is required",
            "Always export the workflow factory as the default export: \
             `export default workflow({ … })`. No other value exports are allowed.",
        ),
        (
            "`Input` must be exported",
            "Always include `export type Input = …`. \
             Use `void` if the workflow takes no input.",
        ),
        (
            "`Output` must be exported",
            "Always include `export type Output = …`. \
             Use `never` if the workflow never returns a value.",
        ),
        (
            "Handler I/O type naming convention",
            "For each handler that has a non-void input, export \
             `export type {HandlerName}Input = …`. \
             For handlers with a non-void return, export \
             `export type {HandlerName}Output = …`. \
             The `{HandlerName}` prefix must match the handler key exactly.",
        ),
        (
            "No other exports allowed",
            "Only `export default`, `export type`, and `export interface` are permitted. \
             Forbidden: value exports (`export const`, `export function`, `export class`), \
             re-exports via `export { … }`, and wildcard re-exports (`export * from '…'`). \
             Type exports must be `Input`, `Output`, or end with `Input` / `Output` \
             (for handler-specific types).",
        ),
    ]
}

#[cfg(feature = "unit_technical_reviewer")]
use crate::services::unit_technical_reviewer::rule_checker::{
    Declaration, Rule, RuleInput, Statement, export_all_source, get_export_default_call_name,
    has_export_default, has_exported_type, module_export_name_str,
};

#[cfg(feature = "unit_technical_reviewer")]
pub fn rules() -> Vec<Rule> {
    vec![
        workflow_default_exported,
        input_exported,
        output_exported,
        no_other_exports,
    ]
}

#[cfg(feature = "unit_technical_reviewer")]
fn workflow_default_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if !has_export_default(program) {
        return vec![
            "Workflow unit must have `export default workflow({ … })` as its default export"
                .to_string(),
        ];
    }
    match get_export_default_call_name(program) {
        Some("workflow") => vec![],
        Some(other) => vec![format!(
            "Workflow unit `export default` must call `workflow(…)`, found call to `{other}`"
        )],
        None => vec![
            "Workflow unit `export default` must be a call to `workflow(…)`, \
             e.g. `export default workflow({ handlers: { … } })`"
                .to_string(),
        ],
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn input_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if has_exported_type(program, "Input") {
        vec![]
    } else {
        vec![
            "Workflow unit must export `export type Input` \
             (use `void` if the workflow takes no input)"
                .to_string(),
        ]
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn output_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if has_exported_type(program, "Output") {
        vec![]
    } else {
        vec![
            "Workflow unit must export `export type Output` \
             (use `never` if the workflow never returns a value)"
                .to_string(),
        ]
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn no_other_exports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    let mut errors = Vec::new();

    for stmt in &program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::TSTypeAliasDeclaration(alias) => {
                            let name = alias.id.name.as_str();
                            let allowed = name == "Input"
                                || name == "Output"
                                || name.ends_with("Input")
                                || name.ends_with("Output");
                            if !allowed {
                                errors.push(format!(
                                    "Workflow unit must not export type `{name}`; \
                                     only `Input`, `Output`, and handler-specific \
                                     `{{HandlerName}}Input` / `{{HandlerName}}Output` are allowed"
                                ));
                            }
                        }
                        Declaration::TSInterfaceDeclaration(iface) => {
                            let name = iface.id.name.as_str();
                            let allowed = name == "Input"
                                || name == "Output"
                                || name.ends_with("Input")
                                || name.ends_with("Output");
                            if !allowed {
                                errors.push(format!(
                                    "Workflow unit must not export interface `{name}`; \
                                     only `Input`, `Output`, and handler-specific \
                                     `{{HandlerName}}Input` / `{{HandlerName}}Output` are allowed"
                                ));
                            }
                        }
                        _ => errors.push(
                            "Workflow unit must not have value exports \
                             (`export const`, `export function`, `export class`, etc.)"
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
                        "Workflow unit must not re-export names via `export {{ … }}`; \
                         found: {}",
                        names.join(", ")
                    ));
                }
            }
            Statement::ExportDefaultDeclaration(_) => {}
            Statement::ExportAllDeclaration(e) => errors.push(format!(
                "Workflow unit must not use `export * from '{}'`",
                export_all_source(e)
            )),
            _ => {}
        }
    }

    errors
}
