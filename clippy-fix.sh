#!/usr/bin/env bash
# Run clippy --fix without the rustc-wrapper so Cargo's compiler probe doesn't hit
# the "multiple input filenames" bug. Normal cargo build/run still use the wrapper (clippy).
set -e
CARGO_BUILD_RUSTC_WRAPPER= exec cargo clippy --fix "$@"
