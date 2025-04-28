#!/usr/bin/env bash
set -euo pipefail

export PYTHONPATH=.

# Define pairs
INPUT_FILES=(
  "static/vectordotdev_vector_issues.json"
  "static/vectordotdev_vrl_issues.json"
)
ENV_FILES=(
  "vector.env"
  "vrl.env"
)
  DB_FILES=(
  "out/db/vectordotdev_vector_issues.db"
  "out/db/vectordotdev_vector_prs.db"
)

# Check that arrays are same length
if [[ ${#INPUT_FILES[@]} -ne ${#ENV_FILES[@]} ]] || [[ ${#INPUT_FILES[@]} -ne ${#DB_FILES[@]} ]]; then
  echo "Error: INPUT_FILES, ENV_FILES, and DB_FILES must have the same length."
  exit 1
fi

# Loop over the pairs
for i in "${!INPUT_FILES[@]}"; do
  input_file="${INPUT_FILES[$i]}"
  env_file="${ENV_FILES[$i]}"
  db_file="${DB_FILES[$i]}"

  echo "Running with input: $input_file and env: $env_file"

  python scripts/db/sqlite_writer.py --input "$input_file" --env-file "$env_file"
  python scripts/db/generate_summary.py --db "${db_file}" --env-file "$env_file"

  START_DATE=$(date -d "$(date +%Y-%m-01) -12 months" +%Y-%m)
  python scripts/util/plot.py --env-file "$env_file" --start "$START_DATE" --input-dir out/summaries --exclude-labels no-changelog
done
