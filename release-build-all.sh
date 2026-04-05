#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

./release-build-main.sh
./release-build-chat-worker.sh
./release-build-unit-technical-reviewer.sh
