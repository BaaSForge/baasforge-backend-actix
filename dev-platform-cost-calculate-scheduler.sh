#!/bin/bash
set -euo pipefail
export CARGO_BUILD_RUSTC_WRAPPER=
CARGO_INCREMENTAL=1 cargo run --bin platform_cost_calculate_scheduler
