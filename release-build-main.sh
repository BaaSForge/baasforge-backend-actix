#!/usr/bin/env bash
# Build the main API server binary in release mode.
# Output: target/release/baal-backend-actix
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "Building main API server (release)..."
CARGO_BUILD_RUSTC_WRAPPER= cargo build --release --bin baal-backend-actix
echo "Done: target/release/baal-backend-actix"
