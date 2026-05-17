// Static checks that unit source has no side effects during module evaluation (import/load time).
//
// Gated by the `unit_module_body_checker` Cargo feature (requires `unit_technical_reviewer` for OXC).
// Call `run_check` from the unit technical reviewer after parsing; merge errors with `run_rule_check`.

#[cfg(feature = "unit_module_body_checker")]
mod checker;

#[cfg(feature = "unit_module_body_checker")]
pub use checker::run_check;

#[cfg(not(feature = "unit_module_body_checker"))]
pub fn run_check(_unit_type: &str, _code: &str) -> Vec<String> {
    vec![]
}

pub fn hint_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "No top-level `await`",
            "Never use `await` at module scope (outside a function body). Async work belongs in \
             `handleRequest`, workflow `run`/handlers, actor handlers, or queue `process`.",
        ),
        (
            "No top-level `sdk` usage",
            "Never reference `sdk` during module initialization. Platform calls must run inside \
             entry callbacks, not when the file is imported.",
        ),
        (
            "No top-level expression statements",
            "Do not run code at import time (`foo();`, IIFEs, `new Client()`, etc.). The module \
             body should only declare imports and exports.",
        ),
        (
            "Module-level `const` must be pure",
            "Non-exported `const` (and `export const` initializers) may only use literals, \
             `as const` strings, simple operators on literals, identifiers, and plain object/array \
             literals of pure values — no function calls, `new`, or `sdk`.",
        ),
        (
            "Workflow: no non-determinism at module scope",
            "In workflow units, do not call `Date.now()`, `Math.random()`, or similar at module \
             scope; use `ctx` APIs inside `run`/handlers instead.",
        ),
        (
            "Executable units: single `export default` factory",
            "Workflow, actor, and queue worker units may call only `workflow(...)`, `actor(...)`, \
             or `queueWorker(...)` at the top level (as `export default`). No other top-level calls.",
        ),
        (
            "Common units: declarations only",
            "Common unit module bodies must contain only `import` and `export` statements — no \
             bare top-level `const`/`let`, expression statements, or `export default`.",
        ),
    ]
}
