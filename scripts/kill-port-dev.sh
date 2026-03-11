#!/bin/bash

set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 PORT" >&2
  exit 1
fi

PORT="$1"

echo "Ensuring no process is listening on port ${PORT}"

if pids=$(lsof -ti :"$PORT" 2>/dev/null); then
  if [ -n "$pids" ]; then
    echo "Killing processes on port $PORT: $pids"
    kill -9 $pids 2>/dev/null || true
  fi
fi

# Wait briefly until the port is really free
for i in 1 2 3 4 5; do
  if ! lsof -ti :"$PORT" >/dev/null 2>&1; then
    break
  fi
  echo "Port $PORT still in use, waiting..."
  sleep 0.5
done

if lsof -ti :"$PORT" >/dev/null 2>&1; then
  echo "WARNING: Port $PORT is still in use after retries." >&2
  exit 1
fi

echo "Port $PORT is free."
