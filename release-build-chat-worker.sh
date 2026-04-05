#!/usr/bin/env bash
# Build the chat_worker binary in release mode.
# Output: target/release/chat_worker
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "Building chat_worker (release)..."
CARGO_BUILD_RUSTC_WRAPPER= cargo build --release --bin chat_worker
echo "Done: target/release/chat_worker"
