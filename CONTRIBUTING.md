# Contributing

## API conventions

### JSON response keys: use snake_case

All JSON keys in API request/response bodies must be **snake_case**, not camelCase.

- **Do:** `stopped_count`, `stopped_message_ids`, `project_id`, `chat_session_id`
- **Don't:** `stoppedCount`, `stoppedMessageIds`, `projectId`, `chatSessionId`

This applies to:
- Manually built payloads (e.g. `json!({ "stopped_count": n })`)
- Any API contract (OpenAPI, docs, or client expectations)

Serialization of structs may use `#[serde(rename_all = "snake_case")]` where appropriate to keep Rust fields and JSON keys consistent.


### Database ID conventions: use UUID v7 for all table primary keys

All database tables MUST use UUID version 7 as the primary key (`id` field).

- Do **not** use integer or auto-incrementing IDs.
- All new tables and all new rows must use UUID v7 for the `id`.
- Any migration or schema update must respect this rule.

**Apply this to all tables**—every table's `id` field must be a UUID v7.

### Generating new IDs in Rust: use `new_uuid()` only

All new entity IDs must be created with **`crate::libs::ids::new_uuid()`** (uuidv7). Do **not** use the `uuid` crate to create IDs (e.g. no `Uuid::new_v4()`, `Uuid::now_v7()`, etc.).

**Enforcing lint:** The project config runs the compiler through Clippy (`rustc-wrapper = "clippy-driver"`), so **`cargo build`** runs both normal compile and Clippy rules. You need Clippy installed: `rustup component add clippy`. If a build script fails (e.g. version detection), use `CARGO_BUILD_RUSTC_WRAPPER= cargo build` to build without the wrapper. **CI** also runs Clippy on every push/PR.