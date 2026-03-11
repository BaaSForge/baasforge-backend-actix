#!/bin/bash

set -euo pipefail

echo "Starting chat_worker dev watcher"

# Ensure we start from a clean state
./scripts/kill-chat-worker-dev.sh

CARGO_INCREMENTAL=1 RUSTC_WRAPPER="" cargo watch -w src -s '
  echo "---------------- chat_worker reload ----------------"

  ./scripts/kill-chat-worker-dev.sh

  echo "Starting cargo run --bin chat_worker"
  cargo run --bin chat_worker
'
