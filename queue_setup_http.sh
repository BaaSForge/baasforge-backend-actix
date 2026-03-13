#!/usr/bin/env bash
# One-time queue setup via RabbitMQ Management HTTP API.
# Run once per environment. Creates the exchanges/queues the API and chat worker expect.
# Requires: curl. Loads all settings from .env (no defaults in this script).

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

if [ -z "$RABBITMQ_MANAGEMENT_URL" ] || [ -z "$RETRY_DELAY_EXCHANGE" ] || [ -z "$RETRY_DELAY_TTL_MS" ]; then
  echo ".env must set RABBITMQ_MANAGEMENT_URL, RETRY_DELAY_EXCHANGE, RETRY_DELAY_TTL_MS" >&2
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
TTL_MS="$RETRY_DELAY_TTL_MS"
AUTH="-u ${USER}:${PASS}"

echo "Using Management API at $MGMT (vhost /)"
echo "Declaring chat_session_exchange (topic, durable)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/exchanges/$VHOST/chat_session_exchange" \
  -d '{"type":"topic","durable":true,"auto_delete":false,"internal":false,"arguments":{}}'

echo "Declaring $RETRY_EXCHANGE (topic, durable)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/exchanges/$VHOST/$RETRY_EXCHANGE" \
  -d '{"type":"topic","durable":true,"auto_delete":false,"internal":false,"arguments":{}}'

echo "Declaring delay queue delay_3s_chat_session_work_queue (TTL ${TTL_MS}ms, no x-dead-letter-routing-key so original key is preserved)..."
curl -sS -f $AUTH -X PUT -H "Content-Type: application/json" \
  "$MGMT/api/queues/$VHOST/delay_3s_chat_session_work_queue" \
  -d "{\"durable\":true,\"auto_delete\":false,\"arguments\":{\"x-message-ttl\":${TTL_MS},\"x-dead-letter-exchange\":\"chat_session_exchange\"}}"

echo "Binding queue to $RETRY_EXCHANGE with routing_key chat_session.#..."
curl -sS -f $AUTH -X POST -H "Content-Type: application/json" \
  "$MGMT/api/bindings/$VHOST/e/$RETRY_EXCHANGE/q/delay_3s_chat_session_work_queue" \
  -d '{"routing_key":"chat_session.#"}'

echo "Queue setup done (HTTP API)."
