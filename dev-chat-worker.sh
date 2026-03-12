#!/bin/bash
set -euo pipefail

CARGO_INCREMENTAL=1 RUSTC_WRAPPER="" cargo watch -w src -s "cargo run --bin chat_worker"
