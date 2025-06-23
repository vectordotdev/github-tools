#!/usr/bin/env bash
set -euo pipefail

export PYTHONPATH=.

# Define pairs
ENV_FILES=(
  "vector.env"
  "vrl.env"
)
# Loop over the pairs
for i in "${!ENV_FILES[@]}"; do
  python scripts/util/fetch_all_issues_and_prs.py --env-file "${ENV_FILES[$i]}"
done
