# syntax=docker/dockerfile:1

ARG BINARY_NAME=baal-backend-actix

FROM rust:1.93-bookworm AS builder

ARG APP_ENV
ARG BINARY_NAME
ARG CARGO_FEATURES=

RUN apt-get update && apt-get install -y \
    libpq-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY .env.prod .env.stage ./

RUN echo "APP_ENV=$APP_ENV" && \
    if [ "$APP_ENV" = "PROD" ]; then \
      cp .env.prod .env; \
    elif [ "$APP_ENV" = "STAGE" ]; then \
      cp .env.stage .env; \
    else \
      echo "Invalid APP_ENV=$APP_ENV" && exit 1; \
    fi

COPY Cargo.toml Cargo.lock build.rs ./
COPY migrations ./migrations
COPY chat-channel-ws.html ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install diesel_cli --version 2.3.5 --no-default-features --features postgres

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --bin "${BINARY_NAME}"

FROM nginx:1.29-bookworm

ARG APP_ENV

RUN apt-get update && apt-get install -y \
    libpq5 \
    wget \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/.env /app/.env
COPY --from=builder /app/target/release/baal-backend-actix /app/baal-backend-actix
COPY --from=builder /app/migrations /app/migrations
COPY diesel.toml /app/diesel.toml
COPY --from=builder /usr/local/cargo/bin/diesel /usr/local/bin/diesel

COPY docker/nginx.prod.conf docker/nginx.stage.conf /tmp/nginx/
RUN if [ "$APP_ENV" = "PROD" ]; then \
      cp /tmp/nginx/nginx.prod.conf /etc/nginx/conf.d/default.conf; \
    elif [ "$APP_ENV" = "STAGE" ]; then \
      cp /tmp/nginx/nginx.stage.conf /etc/nginx/conf.d/default.conf; \
    else \
      echo "Invalid APP_ENV=$APP_ENV" && exit 1; \
    fi

EXPOSE 80 443

HEALTHCHECK --interval=5s --timeout=5s --start-period=15s --retries=3 \
  CMD wget -q --spider http://127.0.0.1/ || exit 1

CMD ["sh", "-c", "./baal-backend-actix & exec nginx -g 'daemon off;'"]
