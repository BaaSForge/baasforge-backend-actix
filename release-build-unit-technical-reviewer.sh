#!/usr/bin/env bash
# Build the unit_technical_reviewer binary in release mode.
#
# --features unit_technical_reviewer is required. It enables:
#   - oxc_parser / oxc_ast / oxc_span for in-process TypeScript AST parsing
#   - tempfile for temporary virtual filesystem creation
#   - compile-time embedding of all @types/node declaration files (via build.rs)
#
# Without the flag the binary compiles but the compiler check is a no-op stub.
#
# Runtime requirement: `tsc` must be in PATH (e.g. npm install -g typescript).
# If `tsc` is absent the checker falls back to `npx --yes tsc`.
#
# Output: target/release/unit_technical_reviewer
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "Building unit_technical_reviewer (release, --features unit_technical_reviewer)..."
CARGO_BUILD_RUSTC_WRAPPER= cargo build --release --bin unit_technical_reviewer --features unit_technical_reviewer
echo "Done: target/release/unit_technical_reviewer"
