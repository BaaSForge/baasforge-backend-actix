#!/bin/bash

set -euo pipefail

echo "Ensuring no chat_worker binaries are running"

if pids=$(pgrep -f "target/debug/chat_worker" 2>/dev/null); then
  if [ -n "$pids" ]; then
    echo "Killing chat_worker PIDs: $pids"
    kill -9 $pids 2>/dev/null || true
  fi
fi

# Wait briefly until all chat_worker binaries are really gone
for i in 1 2 3 4 5; do
  if ! pgrep -f "target/debug/chat_worker" >/dev/null 2>&1; then
    break
  fi
  echo "chat_worker still running, waiting..."
  sleep 0.5
done

if pgrep -f "target/debug/chat_worker" >/dev/null 2>&1; then
  echo "WARNING: chat_worker is still running after retries." >&2
  exit 1
fi

echo "No running chat_worker binaries detected."
