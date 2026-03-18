# Contributing

## API and backend–client conventions

### JSON keys: snake_case everywhere (API and WebSocket)

**All** JSON sent from backend to client (and in API request/response bodies) must use **snake_case** keys, not camelCase.

- **Do:** `stopped_count`, `stopped_message_ids`, `project_id`, `chat_session_id`, `message_id`, `chunk_index`, `runtime_id`
- **Don't:** `stoppedCount`, `stoppedMessageIds`, `projectId`, `chatSessionId`, `messageId`, `chunkIndex`, `runtimeId`

This applies to:
- REST API responses and request bodies
- WebSocket event payloads (e.g. `src/routes/ws/events.rs`)
- Manually built payloads (e.g. `json!({ "stopped_count": n })`)
- Any API contract (OpenAPI, docs, or client expectations)

Use Rust field names in snake_case and avoid `#[serde(rename = "camelCase")]` so serialization stays snake_case. For enums you can use `#[serde(rename_all = "snake_case")]` to keep variant names in JSON as snake_case.

**Developers and AI agents:** When adding or changing API or WebSocket payloads, keep all JSON keys in snake_case.


### Database ID conventions: use UUID v7 for all table primary keys

All database tables MUST use UUID version 7 as the primary key (`id` field).

- Do **not** use integer or auto-incrementing IDs.
- All new tables and all new rows must use UUID v7 for the `id`.
- Any migration or schema update must respect this rule.

**Apply this to all tables**—every table's `id` field must be a UUID v7.

### Generating new IDs in Rust: use `new_uuid()` only

All new entity IDs must be created with **`crate::libs::ids::new_uuid()`** (uuidv7). Do **not** use the `uuid` crate to create IDs (e.g. no `Uuid::new_v4()`, `Uuid::now_v7()`, etc.).

**Enforcing lint:** The project config runs the compiler through Clippy (`rustc-wrapper = "clippy-driver"`), so **`cargo build`** runs both normal compile and Clippy rules. You need Clippy installed: `rustup component add clippy`. If a build script fails (e.g. version detection), use `CARGO_BUILD_RUSTC_WRAPPER= cargo build` to build without the wrapper. **CI** also runs Clippy on every push/PR.