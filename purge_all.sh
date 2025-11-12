#!/bin/bash

# Master script to run all purge operations

set -e

ENV_FILE="${1:-vector.env}"
DRY_RUN_FLAG="--no-dry-run"

# Check for --dry-run flag
if [[ "$2" == "--dry-run" ]]; then
    DRY_RUN_FLAG="--dry-run"
fi

# Get directories
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PURGE_DIR="$SCRIPT_DIR/scripts/purge"

export PYTHONPATH="$SCRIPT_DIR"

echo "Running all purge scripts with env file: $ENV_FILE"
[[ -n "$DRY_RUN_FLAG" ]] && echo "(DRY RUN MODE)"
echo ""

python3 "$PURGE_DIR/purge_nighlties.py" --env-file "$ENV_FILE" --older-than 30 $DRY_RUN_FLAG
echo ""

python3 "$PURGE_DIR/purge_untagged_github_images.py" --env-file "$ENV_FILE" --older-than 30 $DRY_RUN_FLAG
echo ""

python3 "$PURGE_DIR/purge_dockerhub_vector_dev_images.py" --env-file "$ENV_FILE" --older-than 30 $DRY_RUN_FLAG

echo ""
echo "✅ All purge scripts completed"
