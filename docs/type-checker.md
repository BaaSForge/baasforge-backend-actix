# TypeScript Type Checker

The compiler check sub-task in the unit technical reviewer runs a real TypeScript
type check against every unit's code before it is persisted to the database. This
document describes how the checker is structured, what it supports, and how to
build and run it.

## Overview

When the reviewer processes a `create_unit_compiler_check` or
`modify_unit_compiler_check` message it calls
`type_checker::run_typescript_check(&code)`. The function runs a three-stage
pipeline:

1. **In-process AST parse (oxc)** — catches syntax errors immediately without
   spawning a subprocess.
2. **Import validation** — rejects any `node:*` specifier that is not in the
   supported list before touching the filesystem.
3. **`tsc --noEmit` subprocess** — writes a virtual filesystem to a `tempfile`
   directory and runs the TypeScript compiler for full type checking.

On any failure the diagnostic string is written to `payload.error` and
`trigger_llm_continuation` is called so the LLM can react to and fix the problem.

## Feature flag

The real type checker is gated behind the Cargo feature
`unit_technical_reviewer`. Without that feature the function compiles to a no-op
that always returns `Ok(())`. This keeps the `chat_worker` and main API binaries
smaller — they never pull in the oxc crates or the embedded `@types/node` data.

| Binary | Feature required | Type checker active |
|---|---|---|
| `baal-backend-actix` | — | no (no-op) |
| `chat_worker` | — | no (no-op) |
| `unit_technical_reviewer` | `unit_technical_reviewer` | **yes** |

The feature enables four optional Cargo dependencies:

| Crate | Purpose |
|---|---|
| `oxc_allocator` | Bump arena allocator required by the oxc parser |
| `oxc_parser` | TypeScript/JavaScript AST parser |
| `oxc_ast` | AST node types (e.g. `Statement::ImportDeclaration`) |
| `oxc_span` | `SourceType` enum used to tell the parser this is TypeScript |
| `tempfile` | Creates a self-cleaning temporary directory for the virtual FS |

## Build script (`build.rs`)

`build.rs` runs at compile time and walks
`src/services/unit_technical_reviewer/@types/node/` recursively, collecting
every `.d.ts` file. It generates `$OUT_DIR/node_type_files.rs` which looks like:

```rust
static NODE_TYPE_FILE_DATA: &[(&str, &str)] = &[
    ("buffer.d.ts", include_str!("/absolute/path/@types/node/buffer.d.ts")),
    ("crypto.d.ts", include_str!("/absolute/path/@types/node/crypto.d.ts")),
    // … all other .d.ts files …
];
```

`type_checker.rs` includes this file with:

```rust
#[cfg(feature = "unit_technical_reviewer")]
include!(concat!(env!("OUT_DIR"), "/node_type_files.rs"));
```

The result is that every `@types/node` declaration file is **embedded in the
binary** at compile time. There is no runtime filesystem dependency on the source
tree. On first use the array is loaded into a `OnceLock<HashMap>` in RAM and
reused for every subsequent check.

## Supported `node:*` imports

Only the following specifiers are allowed. Any other `node:*` import is rejected
at the import-validation stage (before `tsc` is even spawned) with a clear error
message that lists the allowed set:

| Specifier | Types source |
|---|---|
| `node:http` | `@types/node/http.d.ts` |
| `node:querystring` | `@types/node/querystring.d.ts` |
| `node:stream/consumers` | `@types/node/stream/consumers.d.ts` |
| `node:url` | `@types/node/url.d.ts` |
| `node:crypto` | `@types/node/crypto.d.ts` |
| `node:buffer` | `@types/node/buffer.d.ts` |
| `node:util` | `@types/node/util.d.ts` |
| `node:timers/promises` | `@types/node/timers/promises.d.ts` |

All `@types/node` declaration files (not just the eight above) are written to the
temp directory so that internal cross-references between the declaration files
resolve correctly. The restriction is enforced by the import-validation step in
Rust, not by omitting files.

## `@units/*` imports

Imports from `@units/*` are stubs for the time being. The tsconfig written to the
temp directory maps `@units/*` to a local `units-stub.d.ts`:

```typescript
// units-stub.d.ts
export type Sample = {};
```

An ambient wildcard declaration is also written to `global.d.ts` so TypeScript
accepts any `@units/*` path:

```typescript
declare module "@units/*" {
    export type Sample = {};
}
```

The LLM is instructed to use `Sample` as a placeholder type when importing from
`@units/*`. Specific named exports that are not `Sample` will produce a type
error. This section will be completed later when unit types are fetched from the
database and used as the actual stub content.

## Virtual filesystem layout

For each check the following structure is created in a `tempfile::TempDir`
(automatically deleted on drop):

```
<tmp>/
  main.ts                        ← the LLM's code
  global.d.ts                    ← declare module "@units/*" { export type Sample = {}; }
  units-stub.d.ts                ← export type Sample = {};
  tsconfig.json                  ← compiler options (see below)
  node_modules/
    @types/
      node/
        package.json             ← { "name": "@types/node", "types": "index.d.ts" }
        index.d.ts               ← (embedded)
        http.d.ts                ← (embedded)
        crypto.d.ts              ← (embedded)
        …                        ← all other embedded .d.ts files
        stream/
          consumers.d.ts         ← (embedded)
        timers/
          promises.d.ts          ← (embedded)
        compatibility/
          …                      ← (embedded)
```

`tsconfig.json` uses `NodeNext` module resolution so `node:*` imports are
resolved natively against `node_modules/@types/node`:

```json
{
  "compilerOptions": {
    "target": "ESNext",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "noEmit": true,
    "skipLibCheck": false,
    "paths": {
      "@units/*": ["./units-stub.d.ts"]
    }
  },
  "include": ["main.ts", "global.d.ts"]
}
```

## Runtime requirement: `tsc`

The checker spawns `tsc --noEmit` in the temp directory. `tsc` must be available
at runtime. Two fallback strategies are tried in order:

1. `tsc` — the TypeScript compiler in PATH (installed globally via
   `npm install -g typescript`).
2. `npx --yes tsc` — if `tsc` is not found, `npx` is used. On first call it
   downloads TypeScript; subsequent calls use the npm cache.

For production deployments include `typescript` in the Docker image:

```dockerfile
RUN npm install -g typescript
```

## Building

Always pass `--features unit_technical_reviewer` when building or running the
reviewer binary. Without it the type checker is a no-op stub.

```bash
# Development (with hot reload and clippy)
./dev-unit-technical-reviewer.sh

# Release builds (individual)
./release-build-unit-technical-reviewer.sh
./release-build-chat-worker.sh
./release-build-main.sh

# Release builds (all at once)
./release-build-all.sh

# Manual cargo commands
cargo run  --bin unit_technical_reviewer --features unit_technical_reviewer
cargo build --release --bin unit_technical_reviewer --features unit_technical_reviewer
cargo clippy --features unit_technical_reviewer
```

The `chat_worker` and main API server do **not** need the feature:

```bash
cargo run  --bin chat_worker
cargo build --release --bin chat_worker
```

## Error format seen by the LLM

When the check fails, `payload.error` is set to one of:

- **Syntax error** (caught by oxc, no subprocess):
  ```
  Syntax error(s):
  Unexpected token (line 5, col 3)
  ```

- **Unsupported `node:*` import** (caught by import validator, no subprocess):
  ```
  Unsupported node:* import: "node:fs". Allowed: node:http, node:querystring, …
  ```

- **TypeScript type error** (reported by `tsc`):
  ```
  TypeScript type errors:
  /main.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.
  ```

- **`tsc` not found**:
  ```
  TypeScript compiler not found. Install with `npm install -g typescript`. Error: …
  ```

The LLM receives this string in the tool message history and is expected to fix
the code on its next turn.
