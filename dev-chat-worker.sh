#!/bin/bash
set -euo pipefail
# Unset wrapper so `cargo clippy` runs correctly; we rely on explicit clippy for clippy.toml lints (e.g. disallowed_methods / UUID v7)
export CARGO_BUILD_RUSTC_WRAPPER=
# Run full `cargo clippy` then run; clippy must pass (e.g. no uuid::Uuid::new_v4) before the worker runs
CARGO_INCREMENTAL=1 cargo watch -w src -s "cargo clippy && cargo run --bin chat_worker"
