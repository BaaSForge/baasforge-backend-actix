#!/usr/bin/env bash
# One-time queue setup via RabbitMQ Management HTTP API.
# Run once per environment. Creates the exchanges/queues the API and chat worker expect.
# Requires: curl. Loads all settings from .env (no defaults in this script).
# Queue semantics and CHAT_CONSUMER_TIMEOUT_SECS sizing: see docs/chat-queue.md

# IMPORTANT: This file only imports .env file, not .env.local overrides.

set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$SCRIPT_DIR/.env" ]; then
  set -a
  source "$SCRIPT_DIR/.env"
  set +a
else
  echo "Missing .env in $SCRIPT_DIR" >&2
  exit 1
fi

if [ -z "$RABBITMQ_MANAGEMENT_URL" ] || [ -z "$RETRY_DELAY_EXCHANGE" ] || [ -z "$CHAT_CONSUMER_TIMEOUT_SECS" ]; then
  echo ".env must set RABBITMQ_MANAGEMENT_URL, RETRY_DELAY_EXCHANGE, CHAT_CONSUMER_TIMEOUT_SECS" >&2
  exit 1
fi

# Parse user:pass from RABBIT_URL (amqp://USER:PASS@host/...)
if [ -z "$RABBIT_URL" ]; then
  echo ".env must set RABBIT_URL" >&2
  exit 1
fi
USER=$(sed -n 's|.*://\([^:]*\):.*@.*|\1|p' <<< "$RABBIT_URL")
PASS=$(sed -n 's|.*://[^:]*:\([^@]*\)@.*|\1|p' <<< "$RABBIT_URL")
if [ -z "$USER" ] || [ -z "$PASS" ]; then
  echo "Could not parse user/password from RABBIT_URL" >&2
  exit 1
fi

VHOST="%2F"
MGMT="$RABBITMQ_MANAGEMENT_URL"
RETRY_EXCHANGE="$RETRY_DELAY_EXCHANGE"
AUTH="-u ${USER}:${PASS}"

echo "Using Management API at $MGMT (vhost /)"
# Chat work exchange (main) and work queue — API publishes here, worker consumes from work queue
echo "Declaring chat_session_exchange (topic, durable)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/exchanges/$VHOST/chat_session_exchange" \
  -d '{"type":"topic","durable":true,"auto_delete":false,"internal":false,"arguments":{}}'

# x-consumer-timeout: if a consumer doesn't ack within this time (ms), broker closes the channel and requeues (e.g. after crash). CHAT_CONSUMER_TIMEOUT_SECS in .env — size for one queue message (see docs/chat-queue.md).
# Note: if the queue already exists without this argument, you must delete and recreate it for the argument to apply.
CHAT_CONSUMER_TIMEOUT_MS=$((CHAT_CONSUMER_TIMEOUT_SECS * 1000))
echo "Declaring chat_session_work_queue (durable, consumer_timeout=${CHAT_CONSUMER_TIMEOUT_SECS}s)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/queues/$VHOST/chat_session_work_queue" \
  -d "{\"durable\":true,\"auto_delete\":false,\"arguments\":{\"x-consumer-timeout\":${CHAT_CONSUMER_TIMEOUT_MS}}}"

echo "Binding chat_session_work_queue to chat_session_exchange with routing_key chat_session.#..."
curl -sS -f $AUTH -X POST -H "Content-Type: application/json" \
  "$MGMT/api/bindings/$VHOST/e/chat_session_exchange/q/chat_session_work_queue" \
  -d '{"routing_key":"chat_session.#"}'

# Retry delay exchange (failed messages go here with per-message TTL, then DLX back to chat_session_exchange)
echo "Declaring $RETRY_EXCHANGE (topic, durable)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/exchanges/$VHOST/$RETRY_EXCHANGE" \
  -d '{"type":"topic","durable":true,"auto_delete":false,"internal":false,"arguments":{}}'

# Retry delay queue: no queue-level TTL (per-message expiration only). DLX back to chat_session_exchange.
echo "Declaring retry_delay_chat_session_work_queue (per-message TTL only, DLX to chat_session_exchange)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/queues/$VHOST/retry_delay_chat_session_work_queue" \
  -d '{"durable":true,"auto_delete":false,"arguments":{"x-dead-letter-exchange":"chat_session_exchange"}}'

echo "Binding retry_delay_chat_session_work_queue to $RETRY_EXCHANGE with routing_key chat_session.#..."
curl -sS -f $AUTH -X POST -H "Content-Type: application/json" \
  "$MGMT/api/bindings/$VHOST/e/$RETRY_EXCHANGE/q/retry_delay_chat_session_work_queue" \
  -d '{"routing_key":"chat_session.#"}'

# Dead letter: messages that exceeded max retries (no TTL; process/log later)
echo "Declaring chat_session_dead_letter_exchange (topic, durable)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/exchanges/$VHOST/chat_session_dead_letter_exchange" \
  -d '{"type":"topic","durable":true,"auto_delete":false,"internal":false,"arguments":{}}'

echo "Declaring chat_session_dead_letter_queue (durable, no TTL)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/queues/$VHOST/chat_session_dead_letter_queue" \
  -d '{"durable":true,"auto_delete":false,"arguments":{}}'

echo "Binding chat_session_dead_letter_queue to chat_session_dead_letter_exchange with routing_key chat_session.#..."
curl -sS -f $AUTH -X POST -H "Content-Type: application/json" \
  "$MGMT/api/bindings/$VHOST/e/chat_session_dead_letter_exchange/q/chat_session_dead_letter_queue" \
  -d '{"routing_key":"chat_session.#"}'

echo "Queue setup done (HTTP API)."
