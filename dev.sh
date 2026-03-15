#!/bin/bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 PORT"
  exit 1
fi

export PORT="$1"
# Unset wrapper so `cargo clippy` runs correctly; we rely on explicit clippy for clippy.toml lints (e.g. disallowed_methods / UUID v7)
export CARGO_BUILD_RUSTC_WRAPPER=
# Run full `cargo clippy` then run; clippy must pass (e.g. no uuid::Uuid::new_v4) before the app runs
# -x prepends "cargo ", so use "clippy && cargo run" to get "cargo clippy && cargo run"
CARGO_INCREMENTAL=1 PORT="$PORT" cargo watch -w src -x "clippy && cargo run"
