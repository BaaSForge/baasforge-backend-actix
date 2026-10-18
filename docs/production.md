# Production and self-hosting

This repository contains deployment examples for the current BaaSForge
servers. Review and adapt them before you use them on another server.

## Production topology

A complete deployment needs:

- Five application images from this repository
- PostgreSQL
- RabbitMQ with the required topology
- Redis
- NATS JetStream
- Jaeger, if trace queries are enabled
- Nginx or another TLS reverse proxy
- The separate
  [baasforge-runtime-actix](https://github.com/BaaSForge/baasforge-runtime-actix)
  deployment

`compose.prod.services.yml` starts only PostgreSQL, RabbitMQ, Redis, NATS, and
Jaeger. It does not start the API, workers, nginx, or runtime.

## Security before deployment

**The credentials and URLs committed in `.env*`, Compose files, and nginx files
are project defaults and examples. Do not use them as production secrets.**

Before deployment:

1. Replace all PostgreSQL, RabbitMQ, and NATS credentials.
2. Replace JWT, runtime private API, OAuth, LLM, Stripe, and webhook secrets.
3. Set production database, Redis, RabbitMQ, NATS, runtime, Jaeger, and public
   URLs.
4. Replace the nginx domains and certificate paths.
5. Keep production values in the server secret store or deployment platform.
6. Do not commit or bake real production secrets into an image.

The application accepts process environment variables. The included GitHub
deployment passes secret values to containers with Docker environment options.

## Docker images

The deployment workflow builds these images:

| Process | Dockerfile |
| --- | --- |
| API | `Dockerfile` |
| Chat worker | `docker/Dockerfile.chat_worker` |
| Unit technical reviewer | `docker/Dockerfile.unit_technical_reviewer` |
| Platform-cost consumer | `docker/Dockerfile.platform_cost_calculate_consumer` |
| Platform-cost scheduler | `docker/Dockerfile.platform_cost_calculate_scheduler` |

Each Dockerfile uses a Rust builder and a smaller Debian runtime image. The
technical-reviewer build installs the TypeScript dependencies that `build.rs`
embeds or uses.

`APP_ENV=PROD` selects `.env.prod` during the image build.
`APP_ENV=STAGE` selects `.env.stage`. Runtime environment values override the
built-in `.env` values when no `.env.local` exists in the container.

## Manual self-host sequence

### 1. Prepare the host

Install Docker, Docker Compose, and a TLS reverse proxy. Ensure that application
containers can reach each configured service URL.

### 2. Start support services

Change every credential in `compose.prod.services.yml`, then run:

```bash
docker compose -f compose.prod.services.yml up -d
```

The file exposes:

- PostgreSQL on host port `2563`
- RabbitMQ on `3567`, with management UI on `31567`
- Redis on `9637`
- NATS client on `2422`, monitoring on `2822`, and cluster traffic on `2622`
- Jaeger UI on `61668`, OTLP gRPC on `7431`, and OTLP HTTP on `8431`

Restrict these ports with the host firewall. Do not expose data services to the
public internet.

### 3. Start the runtime

Deploy
[baasforge-runtime-actix](https://github.com/BaaSForge/baasforge-runtime-actix)
separately. Connect it to the same NATS server. Match the release and
environment subjects, streams, consumers, and private API key. Also match the
usage stream and subject names.

The included nginx configuration expects the runtime HTTP service on
`127.0.0.1:1501`.

### 4. Create RabbitMQ resources

Set production values for `RABBIT_URL`, `RABBITMQ_MANAGEMENT_URL`,
`RETRY_DELAY_EXCHANGE`, and `CHAT_CONSUMER_TIMEOUT_SECS`. Then run:

```bash
./queue_setup_http.sh
```

Run this once for each environment before workers receive jobs. The GitHub
deployment workflow does not run this script.

### 5. Run database migrations

Back up the database. Load the production environment and run:

```bash
./migration-pending.sh
./migration-up.sh
```

Run migrations before new application containers. Migrations are not executed
by application startup.

### 6. Start application processes

Run one long-lived container for the API, chat worker, unit technical reviewer,
and platform-cost consumer. Use `--restart unless-stopped`.

Run the platform-cost scheduler as a short-lived container from cron. The
included deployment uses:

```text
*/3 * * * *
```

Pass the same production environment to all processes. Workers do not expose
HTTP ports. Expose the API on port `1101`.

### 7. Start nginx

`nginx.prod.conf`:

- Redirects HTTP to HTTPS.
- Proxies the API domain to `127.0.0.1:1101`.
- Proxies the runtime domain to `127.0.0.1:1501`.
- Supports WebSocket upgrades.
- Uses long proxy timeouts for streaming work.

Replace the BaaSForge domains and certificate paths. `nginx.prod.sh` is an
example for the current host layout. It is not a general installer.

## GitHub deployment

`.github/workflows/deploy.yml` runs for:

- Tags that match `v<version>-prod.<number>`
- Tags that match `v<version>-stage.<number>`
- Manual runs that select an existing matching tag

The workflow:

1. Resolves the target GitHub environment.
2. Builds and pushes all five images to GHCR.
3. Connects to the server over SSH.
4. Runs `diesel migration run` from the API image.
5. Deploys the API and long-lived workers one at a time.
6. Installs the scheduler cron entry.

The workflow expects GitHub environment secrets for SSH, database, auth,
OAuth, RabbitMQ, Redis, NATS, runtime, Jaeger, LLM, and Stripe settings.

It does not provision:

- The server or firewall
- Support services
- RabbitMQ topology
- Nginx or TLS certificates
- The runtime repository

## CI checks

The repository currently has a deployment workflow only. Docker compilation is
part of image building, but there is no pull-request or push workflow for:

- `cargo fmt`
- Clippy
- `./test.sh`
- Prettier
- A separate TypeScript check

Run the checks in [Contributing](../CONTRIBUTING.md) before creating a release
tag.

## Rollback

Application rollback and database rollback are separate operations.

To roll back application code, deploy the previous image tag. Confirm first
that the previous code supports the current database schema.

The deployment workflow has no automatic database rollback. If a migration
must be reversed:

1. Stop writes or place the service in maintenance mode.
2. Back up the database.
3. Review the target migration `down.sql` for data loss.
4. Use the matching source version and production environment.
5. Run `./migration-down.sh` once for each migration to revert.
6. Deploy the compatible application image.
7. Run smoke tests before restoring traffic.

Do not use migration rollback as the first response to a failed container
start. A forward fix is safer when a down migration deletes data.
