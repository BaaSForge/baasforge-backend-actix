# Contributing

Thank you for contributing to BaaSForge.

## Set up the project

Complete the [local development setup](docs/local-development.md) before you
make a change. The setup includes the runtime repository, infrastructure,
database preparation, migrations, RabbitMQ topology, and TypeScript
dependencies.

Create focused changes. Add a migration when a change modifies the database.
Do not include production credentials in commits, logs, examples, or images.

## Naming

Use `BaaSForge` for new user-facing text and `baasforge` for new internal names.
`baal` is the former platform name.

Legacy identifiers still exist in binary names, database settings, container
names, trace tags, and inline markup. Do not rename them as cleanup. A rename
can break deployment, observability, stored content, or runtime integration.
Change a legacy identifier only as part of a tested compatibility migration.

## Required checks

Format Rust:

```bash
cargo fmt --all -- --check
```

Run Clippy for the default targets and the technical-reviewer features:

```bash
CARGO_BUILD_RUSTC_WRAPPER= cargo clippy --all-targets
CARGO_BUILD_RUSTC_WRAPPER= cargo clippy --all-targets \
  --features unit_technical_reviewer,unit_module_body_checker
```

Apply automatic Clippy fixes with:

```bash
./clippy-fix.sh
```

Run all test sets:

```bash
./test.sh
```

`./test.sh` runs the default targets and then runs them with the technical
reviewer features. Rust compilation and Clippy provide the Rust type checks.
The feature-enabled tests cover the TypeScript unit checker.

Run `npm ci` in `src/tsgo` when its dependencies change or when `build.rs`
reports missing type files.

This repository has no Prettier command. Do not claim that Prettier runs as a
project check. The current GitHub workflow deploys tagged versions, but it does
not run format, Clippy, or test checks. Run the checks locally before a pull
request.

## API and backend-client conventions

### Use snake_case JSON keys everywhere

All JSON sent between the backend and clients must use `snake_case` keys. This
applies to:

- REST API requests and responses
- WebSocket event payloads
- Manually built payloads such as `json!({ "stopped_count": count })`
- OpenAPI contracts, documentation, and client expectations

Examples:

- Use `stopped_count`, `stopped_message_ids`, `project_id`,
  `chat_session_id`, `message_id`, `chunk_index`, and `runtime_id`.
- Do not use `stoppedCount`, `stoppedMessageIds`, `projectId`,
  `chatSessionId`, `messageId`, `chunkIndex`, or `runtimeId`.
- Use Rust field names in `snake_case`.
- Do not use `#[serde(rename = "camelCase")]`.
- Use `#[serde(rename_all = "snake_case")]` for serialized enum variants when
  needed.

Review `src/routes/ws/events.rs` when you change a WebSocket payload.
Developers and AI agents must keep this convention for all new or changed API
and WebSocket payloads.

### Keep WebSocket topics private

A user can subscribe to:

- `user:{their_own_user_id}`
- `chat_session:{their_own_user_id}:{project_id}:{chat_session_id}` when they
  own the chat session

Do not allow a user to subscribe to another user's topic or chat session. The
authorization is in `src/routes/ws/topics.rs`.

All allowed topics are scoped to the authenticated user. Do not relax this
rule. A user must never receive another user's events.

### Use UUID v7 for new entity primary keys

New entity tables must use a UUID v7 `id` primary key.

- Do not use integer or auto-incrementing primary keys.
- Use UUID v7 for all new entity tables and entity rows.
- Keep this rule in every migration and schema change.

Some existing domain tables use non-UUID or composite keys, such as
`app_settings` and `project_units`. These are established schema exceptions,
not examples for new entity tables.

Generate new entity IDs with:

```rust
crate::libs::ids::new_uuid()
```

Do not use the `uuid` crate directly to generate entity IDs. Do not call
constructors such as `Uuid::new_v4()` or `Uuid::now_v7()`. Clippy denies these
methods.

The project sets `rustc-wrapper = "clippy-driver"`, so `cargo build` and
`cargo run` also apply Clippy rules. Install Clippy with:

```bash
rustup component add clippy
```

If the wrapper itself blocks a diagnostic build, disable it for that command:

```bash
CARGO_BUILD_RUSTC_WRAPPER= cargo build
```

The current GitHub workflow does not run Clippy. Local checks are required.

## Database migrations

Load the project environment before direct Diesel commands:

```bash
source ./load-project-env.sh
diesel migration generate descriptive_migration_name
```

Edit both generated files:

- `up.sql` applies the change.
- `down.sql` reverses the change.

Then run:

```bash
./migration-up.sh
./migration-list.sh
./migration-pending.sh
```

Test rollback before you submit:

```bash
./migration-down.sh
./migration-up.sh
```

Keep `src/schema.rs` consistent with the database schema. See
[Local development](docs/local-development.md#run-migrations).

## Queue changes

When you add or change a RabbitMQ exchange, queue, or binding:

1. Update `queue_setup_http.sh`.
2. Update the producer and consumer constants.
3. Update the related document in `docs/`.
4. Test the setup on an empty RabbitMQ data directory.

If `CHAT_CONSUMER_TIMEOUT_SECS` changes, recreate the chat work queue so
RabbitMQ applies the new queue argument.

## Pull request checklist

- The change is limited to one clear purpose.
- Format, Clippy, and tests pass.
- New behavior has tests or a clear test procedure.
- Database changes include reversible migrations.
- API and WebSocket payloads use `snake_case`.
- Documentation matches changed commands, configuration, and behavior.
- The change contains no production credentials.
