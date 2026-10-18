# Local development

## Requirements

- Rust 1.93 or a compatible newer toolchain
- Clippy and rustfmt
- Docker with Docker Compose
- Node.js and npm
- Diesel CLI 2.3.5 with PostgreSQL support
- PostgreSQL client libraries
- `curl`
- `lsof` and `pkill` for `dev-stop.sh`

Install the main tools:

```bash
rustup component add clippy rustfmt
cargo install diesel_cli --version 2.3.5 --no-default-features --features postgres
cd src/tsgo && npm ci && cd ../..
```

On macOS, install `libpq` if Rust cannot link PostgreSQL:

```bash
brew install libpq
export LIBRARY_PATH="$(brew --prefix libpq)/lib"
export PKG_CONFIG_PATH="$(brew --prefix libpq)/lib/pkgconfig"
cargo clean
cargo build
```

Docker provides the development PostgreSQL server. Install the full Homebrew
package with `brew install postgresql` only if you also need a host server or
its additional tools.

To keep the library path, add the paths returned by `brew --prefix libpq` to
`~/.cargo/config.toml`. On Apple Silicon, the usual configuration is:

```toml
[env]
LIBRARY_PATH = "/opt/homebrew/opt/libpq/lib"
PKG_CONFIG_PATH = "/opt/homebrew/opt/libpq/lib/pkgconfig"
```

Do not use `brew link --force libpq` unless you understand how it affects other
PostgreSQL installations.

## Environment

The application loads `.env` first. `.env.local` then overrides it. Shell
scripts use the same order through `load-project-env.sh`.

The committed values are development defaults. Keep private, machine-specific
values in the ignored `.env.local` file. Do not put production secrets in
either file.

See [Configuration](configuration.md) for the variable groups and precedence.

## Start supporting services

```bash
docker compose up -d
```

This starts PostgreSQL, RabbitMQ, Redis, and the development nginx load
balancer. Confirm that the URLs in `.env` or `.env.local` match the Compose
ports and credentials.

NATS, Jaeger, and the runtime application are not part of `compose.yml`.

## Prepare a fresh development database

> **IMPORTANT — DO THIS BEFORE THE FIRST MIGRATION**
>
> Create `app_settings` before you run `./migration-up.sh`:
>
> ```sql
> CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT);
> INSERT INTO app_settings (key, value) VALUES ('env', 'development');
> ```
>
> **If migrations were run first, clear and recreate the development database.
> Then create this table and run the migrations again.**

You can run the SQL in the local PostgreSQL container:

```bash
docker exec -i postgres_db psql -U baal -d baal_db <<'SQL'
CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT);
INSERT INTO app_settings (key, value) VALUES ('env', 'development');
SQL
```

`baal` and `baal_db` are legacy local infrastructure names from before the
platform was renamed to BaaSForge. Keep them unless you also update the
matching Compose and connection settings.

This manual step is for a new development database.

## Run migrations

```bash
./migration-up.sh
./migration-list.sh
./migration-pending.sh
```

The scripts load `.env` and `.env.local`, then run the matching Diesel command.

To revert the latest migration:

```bash
./migration-down.sh
```

To create a migration:

```bash
source ./load-project-env.sh
diesel migration generate create_users_table
```

Replace `create_users_table` with a short description of the schema change.
Edit both `up.sql` and `down.sql`. Test the forward and reverse operations:

```bash
./migration-up.sh
./migration-down.sh
./migration-up.sh
```

## Create RabbitMQ resources

Run this after RabbitMQ is ready and before the API and workers process jobs:

```bash
./queue_setup_http.sh
```

Run it once for each environment. It creates the chat, technical-review, and
platform-cost exchanges, work queues, retry queues, dead-letter queues, and
bindings through the RabbitMQ Management API.

The script loads `.env`, then `.env.local`. Queue setup uses `RABBIT_URL`,
`RABBITMQ_MANAGEMENT_URL`, `RETRY_DELAY_EXCHANGE`, and
`CHAT_CONSUMER_TIMEOUT_SECS`. The workers use `RETRY_DELAY_TTL_MS` when they
publish delayed retries. The management username and password are parsed from
`RABBIT_URL`.

Run it again after topology changes. If `CHAT_CONSUMER_TIMEOUT_SECS` changes,
delete and recreate the existing chat work queue before setup so RabbitMQ can
apply the new argument.

## Connect the runtime

Clone and start
[baasforge-runtime-actix](https://github.com/BaaSForge/baasforge-runtime-actix)
as a separate project.

Set:

- `BASE_RUNTIME_ENDPOINT=http://localhost:1500`, unless the runtime uses a
  different port.
- `RUNTIME_NATS_URL` to the NATS server used by both projects.
- Equal release, environment, and usage subjects and stream names in both
  projects.
- Equal private API keys in both projects.

Follow the runtime repository for its installation and configuration. This
repository does not start it.

## Run application processes

Run one API:

```bash
./dev.sh 1101
```

Add `no-clippy` only when Clippy already passed:

```bash
./dev.sh 1101 no-clippy
```

Run individual background processes:

```bash
./dev-chat-worker.sh
./dev-unit-technical-reviewer.sh
./dev-platform-cost-calculate-consumer.sh
./dev-platform-cost-calculate-scheduler.sh
```

The scheduler publishes one batch and exits. Run it again when you need
another local scheduling pass.

Run the scaled set:

```bash
./dev-scaled.sh
```

It starts:

- API on `1101` and `1102`
- Two chat workers
- Two unit technical reviewers
- Two platform-cost consumers

It does not start the scheduler, runtime, NATS, Jaeger, or Compose services.

Stop local Rust processes:

```bash
./dev-stop.sh
```

Stop supporting containers:

```bash
docker compose down
```

## Hot reload

Hot reload is not active. The `cargo watch` commands in `dev.sh`,
`dev-chat-worker.sh`, and `dev-unit-technical-reviewer.sh` are commented out.
The cost scripts also run without a watcher.

Restart the changed process to load new code:

- API change: restart each `dev.sh` process.
- Chat change: restart `dev-chat-worker.sh`.
- Reviewer or TypeScript-check change: restart
  `dev-unit-technical-reviewer.sh`.
- Cost consumer change: restart its consumer script.
- Scheduler change: run its script again.

## Development nginx

The nginx container listens on `http://localhost:1100` and balances requests
between host ports `1101` and `1102`. Use it with `dev-scaled.sh`.

For a single API process, call `http://localhost:1101` directly. The nginx
upstream still includes port `1102`, so a single-process request through
`1100` can fail when nginx selects the missing process.

## Checks

```bash
cargo fmt --all -- --check
CARGO_BUILD_RUSTC_WRAPPER= cargo clippy --all-targets
CARGO_BUILD_RUSTC_WRAPPER= cargo clippy --all-targets \
  --features unit_technical_reviewer,unit_module_body_checker
./test.sh
```

There is no Prettier command in this repository. See
[Contributing](../CONTRIBUTING.md) for project conventions.
