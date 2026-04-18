pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "`export default queueWorker(...)` is required",
            "Always export the queue worker factory as the default export: \
             `export default queueWorker({ … })`. No other value exports are allowed.",
        ),
        (
            "`Input` must be exported",
            "Always include `export type Input = …` defining the per-message input shape.",
        ),
        (
            "`Output` or `BatchOutput` must be exported (not both)",
            "Regular workers: export `export type Output = …` \
             (use `never` if the worker always throws). \
             Batch workers: export `export type BatchOutput = …` (the whole-batch result) \
             instead of `Output`. Do not export both.",
        ),
        (
            "No other exports allowed",
            "Only `export default`, `export type`, and `export interface` are permitted. \
             Forbidden: value exports (`export const`, `export function`, `export class`), \
             re-exports via `export { … }`, and wildcard re-exports (`export * from '…'`). \
             Allowed type names: `Input`, `Output`, `BatchOutput`.",
        ),
    ]
}

#[cfg(feature = "unit_technical_reviewer")]
use crate::services::unit_technical_reviewer::rule_checker::{
    export_all_source, get_export_default_call_name, has_export_default, has_exported_type,
    module_export_name_str, Declaration, Rule, RuleInput, Statement,
};

#[cfg(feature = "unit_technical_reviewer")]
pub fn rules() -> Vec<Rule> {
    vec![
        queue_worker_default_exported,
        input_exported,
        output_or_batch_output_exported,
        no_other_exports,
    ]
}

#[cfg(feature = "unit_technical_reviewer")]
fn queue_worker_default_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if !has_export_default(program) {
        return vec![
            "Queue worker unit must have `export default queueWorker({ … })` as its default export"
                .to_string(),
        ];
    }
    match get_export_default_call_name(program) {
        Some("queueWorker") => vec![],
        Some(other) => vec![format!(
            "Queue worker unit `export default` must call `queueWorker(…)`, \
             found call to `{other}`"
        )],
        None => vec![
            "Queue worker unit `export default` must be a call to `queueWorker(…)`, \
             e.g. `export default queueWorker({ handler: … })`"
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
            "Queue worker unit must export `export type Input` \
             defining the per-message input shape"
                .to_string(),
        ]
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn output_or_batch_output_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    let has_output = has_exported_type(program, "Output");
    let has_batch = has_exported_type(program, "BatchOutput");

    if has_output && has_batch {
        return vec![
            "Queue worker unit must export either `Output` (regular) or `BatchOutput` (batch), \
             not both"
                .to_string(),
        ];
    }
    if !has_output && !has_batch {
        return vec![
            "Queue worker unit must export `export type Output` (regular worker, use `never` \
             if it always throws) or `export type BatchOutput` (batch worker)"
                .to_string(),
        ];
    }
    vec![]
}

#[cfg(feature = "unit_technical_reviewer")]
fn no_other_exports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    const ALLOWED_TYPES: &[&str] = &["Input", "Output", "BatchOutput"];
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
                                    "Queue worker unit must not export type `{name}`; \
                                     only `Input`, `Output`, and `BatchOutput` are allowed"
                                ));
                            }
                        }
                        Declaration::TSInterfaceDeclaration(iface) => {
                            let name = iface.id.name.as_str();
                            if !ALLOWED_TYPES.contains(&name) {
                                errors.push(format!(
                                    "Queue worker unit must not export interface `{name}`; \
                                     only `Input`, `Output`, and `BatchOutput` are allowed"
                                ));
                            }
                        }
                        _ => errors.push(
                            "Queue worker unit must not have value exports \
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
                        "Queue worker unit must not re-export names via `export {{ … }}`; \
                         found: {}",
                        names.join(", ")
                    ));
                }
            }
            Statement::ExportDefaultDeclaration(_) => {}
            Statement::ExportAllDeclaration(e) => errors.push(format!(
                "Queue worker unit must not use `export * from '{}'`",
                export_all_source(e)
            )),
            _ => {}
        }
    }

    errors
}
