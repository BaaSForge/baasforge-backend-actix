# TypeScript Type Checker

The compiler check sub-task in the unit technical reviewer runs a full
TypeScript type-check against every unit's code before it is persisted to the
database.

**No Node.js runtime. No temp files. No files written to disk.**

## Architecture

```
run_typescript_check(code)
  │
  ├─ OXC (in-process, no I/O)
  │    ├─ syntax errors
  │    └─ node:* import allowlist check
  │
  └─ tsgo IPC (virtual filesystem, no disk writes)
       ├─ spawn: tsgo --api --async --cwd /unit \
       │          --callbacks=readFile,fileExists,directoryExists,getAccessibleEntries
       ├─ serve all file content from in-memory HashMap via JSON-RPC callbacks
       └─ sequence: initialize → updateSnapshot → getDefaultProjectForFile
                    → getSemanticDiagnostics → release
```

## What is checked

| Check | Tool | Notes |
|---|---|---|
| Syntax errors | `oxc_parser` | In-process, fast, no I/O |
| Unsupported `node:*` imports | `oxc_parser` AST walk | Rejects any specifier not in the allowed list |
| TypeScript type errors | `tsgo` IPC | Full type checking, strict mode |

## Virtual filesystem layout

All files are served from memory via tsgo IPC callbacks.
Nothing is written to disk.

```
/unit/
  tsconfig.json
  main.ts                         ← LLM code under test
  node_modules/
    **/*.ts                   ← embedded at build time via build.rs
```

## tsgo IPC protocol

The tsgo binary is invoked with `--api --async`, enabling its JSON-RPC server
over stdin/stdout (same `Content-Length:` framing as LSP).

tsgo requests file content from our process via callback RPCs:
- `readFile([path])` → `{"content": "..."}` (found) or `null` (fall back to real FS)
- `fileExists([path])` → `true` (JSON bool, found) or `null` (fall back to real FS)
- `directoryExists([path])` → `true` (JSON bool, found) or `null` (fall back to real FS)
- `getAccessibleEntries([path])` → `{"files": [...], "directories": [...]}` or `null` (fall back)

Virtual paths are under `/unit/` which does not exist on the real
filesystem. For any path not in our VFS we return `null` so tsgo falls back to
the real FS — this lets it find its own TypeScript lib files (e.g. `lib.es2020.d.ts`)
that live next to the binary.

## tsgo installation

tsgo is Microsoft's Go-based TypeScript native compiler (typescript-go /
Project Corsa). It requires no Node.js runtime at run time.

Run the provided script **once** from the repo root:

```sh
cd tsgo && npm install
```

This does `npm install @typescript/native-preview` locally and copies the
platform-native binary to `src/tsgo/node_modules/<platform>/lib/tsgo` (git-ignored).

The binary is always resolved from **the project root's `src/tsgo/node_modules/<platform>/lib/tsgo`**:

| Context | How the project root is found |
|---|---|
| Production (`ROOT/unit_technical_reviewer`) | `exe.parent()` = `ROOT/` |
| Development (`cargo run` from `ROOT/`) | current working directory = `ROOT/` |

## Feature flag

The checker is activated by the `unit_technical_reviewer` Cargo feature.

| Binary | Feature required | Checker active |
|---|---|---|
| `baal-backend-actix` | — | no — returns `Err` |
| `chat_worker` | — | no — returns `Err` |
| `unit_technical_reviewer` | `unit_technical_reviewer` | **yes** |

Without the feature the function returns `Err` so a forgotten flag is caught at
runtime rather than silently ignored.

The feature enables four optional Cargo dependencies:

| Crate | Purpose |
|---|---|
| `oxc_allocator` | Bump arena allocator required by the OXC parser |
| `oxc_parser` | TypeScript / JavaScript AST parser |
| `oxc_ast` | AST node types (`Statement::ImportDeclaration`, …) |
| `oxc_span` | `SourceType` enum used to configure the parser |

`tsgo` itself is not a Cargo dependency — it is an external native binary
(Go-compiled, no wasm, no V8) stored in `src/tsgo/node_modules/<platform>/lib/tsgo` after running
`cd tsgo && npm install`.

## Supported `node:*` imports

| Specifier |
|---|
| `node:http` |
| `node:querystring` |
| `node:stream/consumers` |
| `node:url` |
| `node:crypto` |
| `node:buffer` |
| `node:util` |
| `node:timers/promises` |

Any other `node:*` specifier is rejected before reaching tsgo.

## Supported npm packages

| Package | Import |
|---|---|
| `qs@^6.14.0` | `import qs from "qs"` (default) or named imports from `"qs"` |

Any other bare npm specifier is rejected before reaching tsgo.

## @types/node embedding

The `src/tsgo/node_modules` directory is collected
by `build.rs` at compile time and embedded as `NODE_MODULES_FILE_DATA` (a static
array of `(&str, &str)` pairs). At runtime these are loaded into the VFS
`HashMap` and served to tsgo via `readFile` callbacks — no disk I/O.

## Error format

**Syntax error** (OXC):
```
Syntax error(s):
Unexpected token (line 5, col 3)
```

**Unsupported node:* import** (OXC AST walk):
```
Unsupported node:* import: "node:fs". Supported: node:http, node:querystring, …
```

**TypeScript type error** (tsgo):
```
TypeScript type errors:
main.ts:12:5 — TS2322: Type 'string' is not assignable to type 'number'.
```

**tsgo not found**:
```
tsgo binary not found at src/tsgo/node_modules/<platform>/lib/tsgo relative to the project root.
Run `cd tsgo && npm install` from the repo root.
```

**Feature flag missing**:
```
TypeScript type checking is disabled. Build unit_technical_reviewer with
--features unit_technical_reviewer.
```

## Runtime dependencies

| Dependency | Required at runtime |
|---|---|
| `tsgo` binary | Yes — run `cd tsgo && npm install` once; |
| Node.js | **No** |
| Temp directory | **No** |
| Real filesystem (for type stubs) | **No** — all served from memory |
