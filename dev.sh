#!/bin/bash

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 PORT"
  exit 1
fi

export PORT="$1"

echo "Starting dev server on port ${PORT}"

# Ensure we start from a clean state
./scripts/kill-port-dev.sh "$PORT"

CARGO_INCREMENTAL=1 RUSTC_WRAPPER="" PORT="$PORT" cargo watch -w src -s '
  echo "----------------------------------------"
  echo "Dev reload: ensuring nothing is listening on $PORT"

  ./scripts/kill-port-dev.sh "$PORT"

  echo "Starting cargo run on port $PORT"
  cargo run
'
