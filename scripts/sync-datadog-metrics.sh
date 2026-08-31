#!/usr/bin/env bash
set -euo pipefail

repo="${1:?usage: sync-datadog-metrics.sh OWNER/REPO [LOOKBACK] [METRIC_PREFIX] [ACTIVITY_WINDOW]}"
lookback="${2:-30d}"
metric_prefix="${3:-github.health}"
activity_window="${4:-30d}"
binary="${GITHUB_TOOLS_BIN:-./target/release/github-tools}"

if [[ ! "${repo}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "invalid repository ${repo@Q}; expected OWNER/REPO" >&2
  exit 2
fi

if [[ ! -x "${binary}" ]]; then
  echo "github-tools binary not found or not executable at ${binary}" >&2
  exit 2
fi

args=(
  sync-metrics
  --repo "${repo}"
  --lookback "${lookback}"
  --activity-window "${activity_window}"
  --prefix "${metric_prefix}"
)

if [[ "${DRY_RUN:-false}" == "true" ]]; then
  args+=(--dry-run)
fi

"${binary}" "${args[@]}"
