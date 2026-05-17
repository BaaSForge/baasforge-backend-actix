pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "Named exports only — no default export",
            "Export shared symbols with named exports only (`export function`, `export const`, \
             `export type`, `export { … }`). \
             `export default` is forbidden.",
        ),
        (
            "Runtime imports only from common units",
            "Value imports from other units must target a `common` unit: \
             `import { fn } from '@units/<context>.<module>.common.<name>'`. \
             Use `import type` for types from any unit type.",
        ),
        (
            "Name every `@units/` import",
            "Always use explicit named bindings — `import { sym }` or `import type { T }`. \
             Never bare `import '@units/…'`, `import * as ns from '@units/…'`, or `import x from '@units/…'`.",
        ),
    ]
}

#[cfg(feature = "unit_technical_reviewer")]
use crate::services::unit_technical_reviewer::rule_checker::{
    Rule, RuleInput, has_export_default,
};

#[cfg(feature = "unit_technical_reviewer")]
pub fn rules() -> Vec<Rule> {
    vec![no_default_export]
}

#[cfg(feature = "unit_technical_reviewer")]
fn no_default_export(input: &RuleInput) -> Vec<String> {
    let Some(program) = input.program else {
        return vec![];
    };
    if has_export_default(program) {
        vec!["Common unit must not use `export default`; use named exports only".to_string()]
    } else {
        vec![]
    }
}
