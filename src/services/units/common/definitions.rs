pub const TITLE: &str = "Common";

pub const BRIEF: &str = r#"
A common unit holds shared TypeScript (functions, constants, types) that other units import at
runtime via `import { … } from '@units/<context>.<module>.common.<unit-name>'`. Use it to
deduplicate logic across API, workflow, actor, and queue worker units in the same project.
"#;

pub const DETAIL: &str = r#"
Common units are library modules — no platform entrypoint (`handleRequest`, `workflow`, etc.).

Exports:
- Any **named** export is allowed: `export function`, `export const`, `export type`, `export { … }`.
- **`export default` is forbidden.**

Imports:
- `import type { … } from '@units/…'` — any unit type (types only).
- `import { … } from '@units/….<common>.…'` — runtime values only from units whose path segment `unit_type` is `common`.
- Other unit kinds (`api`, `workflow`, …) must not be value-imported; use the sdk for runtime coupling for those non-common units.

Path shape: `@units/<context>.<module>.common.<unit_name>` (four segments after `@units/`).
"#;

pub const CODE_EXAMPLES: &str = r#"
"#;

pub const COMMUNICATION: &str = r#"
Common units do not get triggered directly. Other units import their exports
at runtime; orchestration between executable units still uses `sdk.*`.
"#;

pub const RUNTIME: &str = r#"
### Common units at runtime

There are no trigger URLs for common units as they can't be triggered directly.
"#;

pub const METADATA: &str = r#"
"#;
