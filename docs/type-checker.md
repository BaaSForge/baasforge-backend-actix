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
       ├─ spawn: tsgo --api --async --cwd /tsgo-vcheck-baal \
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

## TypeScript compiler options

```json
{
  "target": "ESNext",
  "module": "NodeNext",
  "moduleResolution": "NodeNext",
  "strict": true,
  "noImplicitAny": true,
  "strictNullChecks": true,
  "noEmit": true,
  "skipLibCheck": false
}
```

## Virtual filesystem layout

All files are served from memory via tsgo IPC callbacks.
Nothing is written to disk.

```
/tsgo-vcheck-baal/
  tsconfig.json
  main.ts                         ← LLM code under test
  global.d.ts                     ← ambient @units/* wildcard declaration
  units-stub.d.ts                 ← stub target for paths mapping
  node_modules/
    @types/node/
      package.json
      **/*.d.ts                   ← embedded at build time via build.rs
```

## tsgo IPC protocol

The tsgo binary is invoked with `--api --async`, enabling its JSON-RPC server
over stdin/stdout (same `Content-Length:` framing as LSP).

tsgo requests file content from our process via callback RPCs:
- `readFile([path])` → `{"content": "..."}` or `null` (fall back to real FS)
- `fileExists([path])` → `"true"` or `"false"`
- `directoryExists([path])` → `"true"` or `"false"`
- `getAccessibleEntries([path])` → `{"files": [...], "directories": [...]}`

All virtual paths are under `/tsgo-vcheck-baal/` which does not exist on the
real filesystem, so any path not in our HashMap 404s cleanly.

## tsgo installation

tsgo is Microsoft's Go-based TypeScript native compiler (typescript-go /
Project Corsa). It requires no Node.js runtime at run time.

```sh
# Install globally (puts `tsgo` on PATH)
npm install -g @typescript/native-preview

# Or point to a custom binary
export TSGO_PATH=/path/to/tsgo
```

The binary is looked up via `TSGO_PATH` env var, falling back to `tsgo` in
`$PATH`.

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

`tsgo` itself is not a Cargo dependency — it is an external binary (Go-native,
no wasm, no V8) located via `TSGO_PATH` or `$PATH`.

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

## @types/node embedding

The `src/services/unit_technical_reviewer/@types/node/` directory is collected
by `build.rs` at compile time and embedded as `NODE_TYPE_FILE_DATA` (a static
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

**tsgo not installed**:
```
Failed to spawn tsgo binary 'tsgo': No such file or directory.
Install: npm install -g @typescript/native-preview
Or set TSGO_PATH to the path of the tsgo binary.
```

**Feature flag missing**:
```
TypeScript type checking is disabled. Build unit_technical_reviewer with
--features unit_technical_reviewer.
```

## Build / run

```sh
# Development (watch mode, feature included automatically)
./dev-unit-technical-reviewer.sh

# Release builds
./release-build-unit-technical-reviewer.sh
./release-build-all.sh

# Manual
cargo run   --bin unit_technical_reviewer --features unit_technical_reviewer
cargo build --release --bin unit_technical_reviewer --features unit_technical_reviewer
cargo clippy --features unit_technical_reviewer
```

## Runtime dependencies

| Dependency | Required at runtime |
|---|---|
| `tsgo` binary | Yes — install via npm or set `TSGO_PATH` |
| Node.js | **No** |
| Temp directory | **No** |
| Real filesystem (for type stubs) | **No** — all served from memory |
