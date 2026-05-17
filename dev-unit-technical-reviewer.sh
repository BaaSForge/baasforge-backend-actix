#!/bin/bash
set -euo pipefail
export CARGO_BUILD_RUSTC_WRAPPER=
# The unit_technical_reviewer binary requires --features unit_technical_reviewer to enable
# the oxc-based TypeScript type checker. Without the flag the type checker is a no-op stub.
# CARGO_INCREMENTAL=1 cargo watch -w src -s "cargo run --bin unit_technical_reviewer --features unit_technical_reviewer"
CARGO_INCREMENTAL=1 cargo run --bin unit_technical_reviewer --features unit_technical_reviewer,unit_module_body_checker
