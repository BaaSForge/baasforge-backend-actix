#!/bin/bash

# Load .env first, then .env.local overrides if present
set -a  # Export all subsequent variables
source .env
[ -f .env.local ] && source .env.local
set +a  # Disable exporting

diesel migration revert