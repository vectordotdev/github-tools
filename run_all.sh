#!/usr/bin/env bash
export PYTHONPATH=.
rm -rf ./out
python scripts/db/sqlite_writer.py --input static/20250417_143706_vectordotdev_vector_issues.json
python scripts/db/generate_summary.py --db out/vectordotdev_vector_issues.db

START_DATE=$(date -d "$(date +%Y-%m-01) -12 months" +%Y-%m)
python scripts/util/plot.py --start $START_DATE --input-dir out/summaries --exclude-labels no-changelog
