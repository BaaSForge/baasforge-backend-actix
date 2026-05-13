pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "`export default actor<State>(...)` is required",
            "Always export the actor factory as the default export: \
             `export default actor<State>({ … })`. No other value exports are allowed.",
        ),
        (
            "`State` must be exported",
            "Always include `export type State = …` defining the actor's persistent state shape.",
        ),
        (
            "`onAlarm` and `onDeadLetter` are internal — do NOT export their types",
            "The `onAlarm` and `onDeadLetter` handlers are framework-internal. \
             Do not export `OnAlarmInput`, `OnAlarmOutput`, `OnDeadLetterInput`, \
             or `OnDeadLetterOutput` types.",
        ),
        (
            "Handler I/O type naming convention",
            "For each handler that has a non-void input, export \
             `export type {HandlerName}Input = …`. \
             For handlers with a non-void return, export \
             `export type {HandlerName}Output = …`. \
             `{HandlerName}` is the handler key with its first character uppercased \
             (e.g. `createAgent` → `CreateAgentInput`). \
             When `handlers` is a static object literal, every exported `*Input` / `*Output` must \
             match a key in that object under that rule (omit `handlers` or use a non-literal \
             value if the actor has no externally invoked handlers).",
        ),
        (
            "No other exports allowed",
            "Only `export default`, `export type`, and `export interface` are permitted. \
             Forbidden: value exports (`export const`, `export function`, `export class`), \
             re-exports via `export { … }`, and wildcard re-exports (`export * from '…'`). \
             Type exports must be `State` or end with `Input` / `Output` \
             (for handler-specific types).",
        ),
    ]
}

#[cfg(feature = "unit_technical_reviewer")]
use super::handler_keys::{
    actor_handler_key_to_exported_type_prefix, parse_actor_handlers_literal_keys,
};
#[cfg(feature = "unit_technical_reviewer")]
use crate::services::unit_technical_reviewer::rule_checker::{
    Declaration, Rule, RuleInput, Statement, export_all_source, get_export_default_call_name,
    has_export_default, has_exported_type, module_export_name_str,
};

#[cfg(feature = "unit_technical_reviewer")]
pub fn rules() -> Vec<Rule> {
    vec![
        actor_default_exported,
        state_exported,
        no_alarm_deadletter_exports,
        handler_io_exports_match_handlers,
        no_other_exports,
    ]
}

#[cfg(feature = "unit_technical_reviewer")]
fn actor_default_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if !has_export_default(program) {
        return vec![
            "Actor unit must have `export default actor<State>({ … })` as its default export"
                .to_string(),
        ];
    }
    match get_export_default_call_name(program) {
        Some("actor") => vec![],
        Some(other) => vec![format!(
            "Actor unit `export default` must call `actor(…)`, found call to `{other}`"
        )],
        None => vec![
            "Actor unit `export default` must be a call to `actor<State>(…)`, \
             e.g. `export default actor<State>({ handlers: { … } })`"
                .to_string(),
        ],
    }
}

#[cfg(feature = "unit_technical_reviewer")]
fn state_exported(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if has_exported_type(program, "State") {
        vec![]
    } else {
        vec![
            "Actor unit must export `export type State = …` defining the actor's persistent state"
                .to_string(),
        ]
    }
}

#[cfg(feature = "unit_technical_reviewer")]
const FORBIDDEN_HANDLER_TYPE_PREFIXES: &[&str] = &["OnAlarm", "OnDeadLetter"];

#[cfg(feature = "unit_technical_reviewer")]
fn no_alarm_deadletter_exports(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    let mut errors = Vec::new();
    for stmt in &program.body {
        if let Statement::ExportNamedDeclaration(export) = stmt {
            if let Some(Declaration::TSTypeAliasDeclaration(alias)) = &export.declaration {
                let name = alias.id.name.as_str();
                for prefix in FORBIDDEN_HANDLER_TYPE_PREFIXES {
                    if name.starts_with(prefix) {
                        errors.push(format!(
                            "Actor unit must not export `{name}`; \
                             `onAlarm` and `onDeadLetter` are internal handlers \
                             — their types must not be exported"
                        ));
                    }
                }
            }
        }
    }
    errors
}

#[cfg(feature = "unit_technical_reviewer")]
fn handler_io_exports_match_handlers(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if get_export_default_call_name(program) != Some("actor") {
        return vec![];
    }
    let parsed = parse_actor_handlers_literal_keys(program);
    let Some(handler_keys_opt) = parsed else {
        return vec![];
    };
    let Some(handler_keys) = handler_keys_opt else {
        return vec![];
    };
    let mut expected: std::collections::HashSet<String> = std::collections::HashSet::new();
    for k in &handler_keys {
        expected.insert(actor_handler_key_to_exported_type_prefix(k));
    }
    let mut errors = Vec::new();
    for stmt in &program.body {
        let Statement::ExportNamedDeclaration(export) = stmt else {
            continue;
        };
        let Some(decl) = &export.declaration else {
            continue;
        };
        let name = match decl {
            Declaration::TSTypeAliasDeclaration(a) => a.id.name.as_str(),
            Declaration::TSInterfaceDeclaration(i) => i.id.name.as_str(),
            _ => continue,
        };
        if name == "State" {
            continue;
        }
        let stem = if let Some(s) = name.strip_suffix("Input") {
            s
        } else if let Some(s) = name.strip_suffix("Output") {
            s
        } else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        if FORBIDDEN_HANDLER_TYPE_PREFIXES
            .iter()
            .any(|p| name.starts_with(p))
        {
            continue;
        }
        if !expected.contains(stem) {
            let keys_hint = if handler_keys.is_empty() {
                "(handlers object is empty)".to_string()
            } else {
                handler_keys.join(", ")
            };
            errors.push(format!(
                "Actor unit exports `{name}` but no `handlers` key maps to that type prefix \
                 `{stem}` (expected stem for each key: first character uppercased, rest unchanged; \
                 handler keys found: {keys_hint})"
            ));
        }
    }
    errors
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
                            let allowed = name == "State"
                                || name.ends_with("Input")
                                || name.ends_with("Output");
                            if !allowed {
                                errors.push(format!(
                                    "Actor unit must not export type `{name}`; \
                                     only `State` and handler-specific \
                                     `{{HandlerName}}Input` / `{{HandlerName}}Output` are allowed"
                                ));
                            }
                        }
                        Declaration::TSInterfaceDeclaration(iface) => {
                            let name = iface.id.name.as_str();
                            let allowed = name == "State"
                                || name.ends_with("Input")
                                || name.ends_with("Output");
                            if !allowed {
                                errors.push(format!(
                                    "Actor unit must not export interface `{name}`; \
                                     only `State` and handler-specific \
                                     `{{HandlerName}}Input` / `{{HandlerName}}Output` are allowed"
                                ));
                            }
                        }
                        _ => errors.push(
                            "Actor unit must not have value exports \
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
                        "Actor unit must not re-export names via `export {{ … }}`; \
                         found: {}",
                        names.join(", ")
                    ));
                }
            }
            Statement::ExportDefaultDeclaration(_) => {}
            Statement::ExportAllDeclaration(e) => errors.push(format!(
                "Actor unit must not use `export * from '{}'`",
                export_all_source(e)
            )),
            _ => {}
        }
    }

    errors
}
