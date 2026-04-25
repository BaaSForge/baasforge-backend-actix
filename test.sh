#!/usr/bin/env bash
set -euo pipefail

cargo test --all-targets "$@"
cargo test --all-targets --features unit_technical_reviewer "$@"
