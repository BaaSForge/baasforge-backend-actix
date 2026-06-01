# TypeScript Type Checker

The compiler check sub-task runs TypeScript type-checking against every unit's code before it is persisted to the database.

**No Node.js runtime. No temp files for tsgo. No files written to disk for type-check.**

## Pipeline (create / modify unit)

```
pre_check → compiler_check → lint_check → rule_check → DB insert
```

| Step | Tool | Notes |
|------|------|--------|
| Compiler check | tsgo IPC (in-memory VFS) | Full strict type-check |
| Lint check | OXC + tsgolint headless | `no-explicit-any`, no `import * as …` (syntax) + `no-unsafe-type-assertion` (type-aware; `as ActorInstanceId` / `WorkflowInstanceId` / `QueueWorkerRunId` allowed) |
| Rule check | OXC AST rules | Project conventions |

## Architecture

```
run_typescript_check(code)
  └─ tsgo IPC (virtual filesystem, no disk writes)

run_type_lint_check(code)
  ├─ OXC: no-explicit-any, no namespace imports (`import * as …`)
  └─ tsgolint headless: no-unsafe-type-assertion (except `as` to SDK branded IDs)
       (temp dir: main.ts + @units + symlink to src/tsgo/node_modules)
```

## tsgo installation

Run once from the repo root:

```sh
cd src/tsgo && npm install
```

Installs `@typescript/native-preview` (tsgo) and `@oxlint-tsgolint/*` (tsgolint binary per platform).

## tsgolint (type-aware lint)

Uses the same **typescript-go** stack as tsgo. Invoked as `tsgolint headless` with JSON on stdin (same protocol as oxlint's type-aware mode). No Node.js at runtime.

Override binary path: `OXLINT_TSGOLINT_PATH=/path/to/tsgolint`

## Feature flag

Activated by the `unit_technical_reviewer` Cargo feature on the `unit_technical_reviewer` binary.
