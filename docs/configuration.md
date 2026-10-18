# Configuration

## Load order

Rust processes load configuration in this order:

1. Existing process environment
2. `.env`, which fills values that are not set
3. `.env.local`, which overrides earlier values

Migration and queue scripts source `.env`, then `.env.local`.

Only `.env.local` has special override behavior. Files such as
`.env.local.prod` are not loaded automatically.

The environment files are also shell files because `load-project-env.sh`
sources them. Use simple `KEY=value` lines. Do not add shell-incompatible
values or quote styles without testing every script.

## File roles

| File | Role |
| --- | --- |
| `.env` | Local development defaults |
| `.env.local` | Ignored local secrets and machine-specific overrides |
| `.env.stage` | Values copied into staging images at build time |
| `.env.prod` | Values copied into production images at build time |

Docker builds select `.env.stage` or `.env.prod` from the `APP_ENV` build
argument and copy it into the image as `.env`.

The deployment workflow supplies sensitive values as process environment
variables. These values override the image `.env` because production
containers do not contain `.env.local`.

## Security

**Treat all committed credentials as development defaults. Replace them before
production.**

Do not:

- Use committed PostgreSQL, RabbitMQ, or NATS passwords in production.
- Put production JWT, OAuth, LLM, runtime, Stripe, or webhook secrets in Git.
- Publish real secrets in Docker image layers.
- Log full connection URLs when they contain credentials.

Use GitHub environment secrets, a server secret manager, or an equivalent
deployment system. Rotate a value if it was published.

## Core application

- `APP_ENV`: application mode. Current modes are `staging` and `production`.
- `BIND_HOST`: interface for the Actix server.
- `PORT`: HTTP port. `dev.sh` overrides it with its first argument.
- `DATABASE_URL`: PostgreSQL connection URL.
- `DATABASE_POOL_SIZE`: connections per process.
- `DATABASE_CONNECTION_TIMEOUT_SECS`: pool connection timeout.
- `DATABASE_POOL_IDLE_TIMEOUT_SECS`: idle connection lifetime.
- `CORS_ALLOWED_ORIGINS`: comma-separated browser origins.

Total database connections increase with every API and worker replica. Keep
`replica count × DATABASE_POOL_SIZE` below the PostgreSQL connection limit.

## Authentication and payments

Required token values:

- `AUTH_ACCESS_TOKEN_SECRET`
- `AUTH_REFRESH_TOKEN_SECRET`

OAuth settings are optional groups. Set every value in a group when you enable
that provider:

- `GOOGLE_OAUTH_CLIENT_ID`
- `GOOGLE_OAUTH_CLIENT_SECRET`
- `GOOGLE_OAUTH_REDIRECT_URI`
- `GOOGLE_OAUTH_STATE_SECRET`
- `APP_GITHUB_OAUTH_CLIENT_ID`
- `APP_GITHUB_OAUTH_CLIENT_SECRET`
- `APP_GITHUB_OAUTH_REDIRECT_URI`
- `APP_GITHUB_OAUTH_STATE_SECRET`

Stripe requires:

- `STRIPE_SECRET_KEY`
- `STRIPE_WEBHOOK_SECRET`
- `STRIPE_CURRENCY`
- `STRIPE_SUCCESS_URL`
- `STRIPE_CANCEL_URL`

## RabbitMQ

Application and worker settings:

- `RABBIT_URL`
- `RABBIT_OPERATION_TIMEOUT_SECS`
- `RABBIT_WORKER_RECOVERY_WAIT_SECS`
- `RABBIT_CONNECT_RETRY_INTERVAL_SECS`
- `RETRY_DELAY_EXCHANGE`
- `RETRY_DELAY_TTL_MS`
- `CHAT_CONSUMER_TIMEOUT_SECS`

`RABBITMQ_MANAGEMENT_URL` is used by `queue_setup_http.sh`. It is not part of
the Rust `Settings` object.

The queue setup script gets the management username and password from
`RABBIT_URL`.

## Redis

- `REDIS_URL`: Redis connection URL.
- `STOPPED_MESSAGES_IDS_CACHE_TTL_SECS`: lifetime of stopped-message markers.

Redis carries WebSocket pub/sub traffic and transient chat coordination state.

## LLM providers

- `LLM_PROVIDER`: selected provider.
- `OPENAI_API_KEY`, `OPENAI_MODEL`, `OPENAI_REASONING_EFFORT`
- `GEMINI_API_KEY`, `GEMINI_MODEL`, `GEMINI_THINKING_ENABLED`,
  `GEMINI_THINKING_LEVEL`, `GEMINI_THINKING_BUDGET`
- `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`,
  `ANTHROPIC_THINKING_ENABLED`, `ANTHROPIC_THINKING_BUDGET_TOKENS`

Error simulation is for development:

- `LLM_SIMULATE_ERROR_AFTER_CALLS`
- `UNIT_REVIEWER_SIMULATE_FAILURE`

Do not enable simulation values in production.

## Runtime and NATS

HTTP connection:

- `BASE_RUNTIME_ENDPOINT`: public runtime URL, without a trailing slash.
- `BASE_PRIVATE_ENDPOINT`: public URL of this API's private runtime proxy.
- `RUNTIME_PRIVATE_API_KEY`: shared key for private runtime requests.

NATS connection and release control:

- `RUNTIME_NATS_URL`
- `RUNTIME_NATS_SUBJECT_RELEASE`
- `RUNTIME_NATS_JETSTREAM_STREAM_RELEASE`
- `RUNTIME_NATS_JETSTREAM_CONSUMER_RELEASE`
- `RUNTIME_NATS_SUBJECT_ENV`
- `RUNTIME_NATS_JETSTREAM_STREAM_ENV`
- `RUNTIME_NATS_JETSTREAM_CONSUMER_ENV`
- `RUNTIME_NATS_OPERATION_TIMEOUT_SECS`

Usage publishing:

- `NATS_JETSTREAM_STREAM_USAGE`
- `NATS_SUBJECT_USAGE_BATCH`
- `USAGE_PUBLISH_BATCH_SIZE`
- `USAGE_PUBLISH_FLUSH_MS`
- `USAGE_PUBLISH_CHANNEL_CAPACITY`

The backend and
[runtime repository](https://github.com/BaaSForge/baasforge-runtime-actix)
must use the same NATS URL and matching subject, stream, and consumer names.

## Platform cost jobs

- `PLATFORM_COST_CALCULATE_SCHEDULER_BATCH_SIZE`: maximum projects in one
  scheduler run.
- `PLATFORM_COST_CALCULATE_WINDOW_MINUTES`: size of each usage window.

Both values must be greater than zero.

## Observability

- `JAEGER_QUERY_BASE_URL`: Jaeger Query API base URL, without a trailing slash.

The API uses it to proxy trace lookups. The production support Compose file
also exposes Jaeger OTLP ports for trace collection.

## Compose credentials

`compose.yml` and `compose.prod.services.yml` contain their own PostgreSQL,
RabbitMQ, and NATS credentials. Application connection URLs must match those
values.

Before production, replace the credentials in the production Compose file and
set matching secret connection URLs for every application process. Do not rely
on local defaults.
