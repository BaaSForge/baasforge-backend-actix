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
