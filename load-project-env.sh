#!/bin/bash
# Load .env first, then .env.local overrides if present (from this script's directory).
_PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
set -a
source "$_PROJECT_ROOT/.env"
[ -f "$_PROJECT_ROOT/.env.local" ] && source "$_PROJECT_ROOT/.env.local"
set +a
unset _PROJECT_ROOT
