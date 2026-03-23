#!/bin/bash
source "$(dirname "${BASH_SOURCE[0]}")/load-project-env.sh"

diesel migration run