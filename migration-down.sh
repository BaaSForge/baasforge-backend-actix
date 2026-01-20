#!/bin/bash

# Load .env variables into the environment
set -a  # Export all subsequent variables
source .env
set +a  # Disable exporting

diesel migration revert