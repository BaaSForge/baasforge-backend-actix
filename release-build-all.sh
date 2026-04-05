#!/usr/bin/env bash
# Build all three binaries in release mode.
#
# Binaries produced:
#   target/release/baal-backend-actix        — main API server
#   target/release/chat_worker               — LLM / chat queue worker
#   target/release/unit_technical_reviewer   — async TypeScript type-checker worker
#
# The unit_technical_reviewer binary is built with --features unit_technical_reviewer
# so the oxc-based type checker and embedded @types/node declarations are included.
# The other two binaries are built without that feature to keep their size smaller.
#
# Runtime requirement for unit_technical_reviewer:
#   tsc must be in PATH (npm install -g typescript), or npx must be available.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "=== Building all release binaries ==="
echo ""

echo "[1/3] Building main API server..."
CARGO_BUILD_RUSTC_WRAPPER= cargo build --release --bin baal-backend-actix
echo "      Done: target/release/baal-backend-actix"
echo ""

echo "[2/3] Building chat_worker..."
CARGO_BUILD_RUSTC_WRAPPER= cargo build --release --bin chat_worker
echo "      Done: target/release/chat_worker"
echo ""

echo "[3/3] Building unit_technical_reviewer (--features unit_technical_reviewer)..."
CARGO_BUILD_RUSTC_WRAPPER= cargo build --release --bin unit_technical_reviewer --features unit_technical_reviewer
echo "      Done: target/release/unit_technical_reviewer"
echo ""

echo "=== All binaries built successfully ==="
echo "  target/release/baal-backend-actix"
echo "  target/release/chat_worker"
echo "  target/release/unit_technical_reviewer"
