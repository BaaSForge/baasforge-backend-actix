#!/bin/bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 PORT"
  exit 1
fi

export PORT="$1"

CARGO_INCREMENTAL=1 RUSTC_WRAPPER="" PORT="$PORT" cargo watch -w src -x run
