# github-tools

> [!NOTE]
> Experimental repo for gaining insights into open source project health.

Tools for extracting data from GitHub, storing it in a local SQLite database, querying it, and visualizing trends.

# Trends

Per-repo interactive dashboards (GitHub Pages):

- [Vector](https://vectordotdev.github.io/github-tools/vector/)
- [VRL](https://vectordotdev.github.io/github-tools/vrl/)
- [Quickwit](https://vectordotdev.github.io/github-tools/quickwit/)
- [Tantivy](https://vectordotdev.github.io/github-tools/tantivy/)

# Directory Layout

```
src/             # Rust source (single binary: github-tools)
docs/            # GitHub Pages — interactive HTML dashboards (ECharts)
data/            # Committed snapshots: JSON inputs
  {owner}_{repo}/issues/  # Issues/PRs JSON split by year (2024.json, 2025.json, ...)
  {owner}_{repo}/discussions/  # Discussions JSON split by year
out/             # Gitignored — all generated and local-only files
  db/            # SQLite databases
  summaries/     # Generated CSVs
  purge/         # Purge audit logs (local only)
```

# Build

```shell
cargo build --release
# Binary: target/release/github-tools
```

# Configuration

Commands read credentials from the environment. Keep them in a single `secrets.env`:

```dotenv
GITHUB_TOKEN=...
DD_API_KEY=...       # Datadog metric submission only
DD_SITE=datadoghq.com
DOCKER_USERNAME=...   # purge commands only
DOCKER_PASSWORD=...   # purge commands only
```

The target repository is always specified explicitly via `--repo org/name`.

If you use a password manager CLI (e.g. `op`), store secret references there and inject at runtime — secrets never touch disk:

```sh
op run --env-file secrets.env -- github-tools fetch-issues --repo vectordotdev/vector
```

Plain text env files still work via `--env-file` for users without a password manager CLI.

# Commands

```
github-tools <COMMAND>

Fetch:
  fetch-all          Fetch issues + discussions for all repos (workflow)
  fetch-issues       Fetch all issues/PRs for a repository
  fetch-discussions  Fetch all discussions for a repository
  fetch-labels       Fetch all labels for a repository

Pipeline:
  sync-metrics       Fetch into data/, rebuild DB, and submit a Datadog snapshot
  push-metrics       Submit a Datadog snapshot from an existing local DB
  generate-all       Build DB + summaries + charts for all repos (workflow)
  build-db           Load issues JSON into SQLite database
  generate-summaries Generate CSV summaries from SQLite database
  generate-charts    Render HTML dashboards into docs/ from out/summaries/

Purge:
  purge-all          Run all purge operations (workflow)
  purge nightly      Purge old nightly images from GitHub and Docker Hub
  purge untagged     Purge untagged GitHub container images
  purge vector-dev   Purge old vector-dev images from Docker Hub

AI review:
  automated-review-stats  Count review bot comments by reaction (liked / disliked / no signal)

Maintenance:
  close-old-prs          Close PRs with 'meta: awaiting author' older than 6 months
  delete-stale-branches  Delete branches with no commits in 4 years
  remove-legacy-label    Remove legacy type labels from issues/PRs
```

Run `github-tools <COMMAND> --help` for full argument details.

# Workflow

## 1. (Optional) Fetch fresh data from GitHub

```shell
for repo in vectordotdev/vector vectordotdev/vrl quickwit-oss/quickwit quickwit-oss/tantivy; do
  op run --env-file secrets.env -- github-tools fetch-all --repo "$repo"
done
```

Fetches merge directly into the committed snapshot layout:

- Issues/PRs: `data/{owner}_{repo}/issues/{year}.json`
- Discussions: `data/{owner}_{repo}/discussions/{year}.json`

Use `--since 30d` (or another ISO/relative value) for an incremental refresh. Fresh records replace matching record numbers, while older snapshot data is retained.

## 2. Generate DB, summaries, and charts

```shell
for repo in vectordotdev/vector vectordotdev/vrl quickwit-oss/quickwit quickwit-oss/tantivy; do
  github-tools generate-all --repo "$repo"
done
```

`generate-all` builds the SQLite DB, generates CSVs into `out/summaries/`, and renders interactive HTML dashboards into `docs/`. Review the diff in `docs/` before committing.

## 3. (Optional) Purge stale container images

```shell
op run --env-file secrets.env -- github-tools purge-all --dry-run
op run --env-file secrets.env -- github-tools purge-all  # omit --dry-run to execute
```

Audit logs written to `out/purge/` (local only).

## 4. AI-assisted review stats

Measures how contributors react to automated review bot comments (👍 liked / 👎 disliked / no signal).

```shell
# Discover the bot's GitHub login (lists all review comment authors by frequency)
op run --env-file secrets.env -- github-tools automated-review-stats \
  --repo vectordotdev/vector --since 3m

# Produce stats + update trends/vector.md
op run --env-file secrets.env -- github-tools automated-review-stats \
  --repo vectordotdev/vector \
  --bot-login "chatgpt-codex-connector" \
  --since 2026-01-01
```

Outputs:
- Console summary (like rate, dislike rate)
- `out/automated-review-stats/{owner}_{repo}.csv` — per-comment table with URL and reaction (gitignored)
- `out/summaries/{owner}_{repo}_automated_review_stats.json` — stats snapshot picked up by `generate-all`

Re-run `generate-all` after collecting stats to update the dashboard with the AI review chart.

## 5. Datadog project-health metrics

`sync-metrics` composes the existing fetch and database commands into the path intended for scheduled automation:

```shell
op run --env-file secrets.env -- github-tools sync-metrics \
  --repo vectordotdev/vector \
  --lookback 30d
```

The command:

1. Fetches issues, pull requests, and discussions updated inside the lookback.
2. Merges them into the appropriate year files under `data/{owner}_{repo}/`.
3. Rebuilds `out/db/{owner}_{repo}.db` from the full committed snapshot.
4. Sends current gauge snapshots to Datadog.

Use `--dry-run` to perform the fetch and database rebuild while printing, but not submitting, the resulting metrics. To preview metrics entirely offline after a database has been built:

```shell
github-tools push-metrics \
  --repo vectordotdev/vector \
  --since 30d \
  --dry-run
```

The default prefix is `github.health`; override it with `--prefix`. The metrics are:

| Metric | Meaning | Important tags |
|---|---|---|
| `github.health.issues` | Current open issue backlog | `repo`, `issue_type`, `age`, labels |
| `github.health.prs` | Current non-draft PR backlog | `repo`, `age`, labels |
| `github.health.discussions` | Current open discussions | `repo`, `category`, `answered`, `age` |
| `github.health.issues.closed` | Issues closed inside the lookback | `repo`, `window`, `issue_type`, labels |
| `github.health.prs.closed` | PRs closed inside the lookback | `repo`, `window`, labels |
| `github.health.discussions.closed` | Discussions closed inside the lookback | `repo`, `window`, `category`, `answered` |

All are gauges sampled at the run time. Rolling-window totals are gauges rather than Datadog `count` metrics because each point is a complete lookback snapshot, not a count accumulated during the reporting interval.

The external scheduler invokes the command once per repository, supplying the appropriate `--repo` and `--lookback`. Its GitHub token needs read access to that repository. Submitting metrics needs `DD_API_KEY`; non-US1 accounts should also set `DD_SITE` (for example, `datadoghq.eu`).

For a GitHub runner, the repository includes a trigger-agnostic composite action at `.github/actions/sync-datadog-metrics`. The calling workflow supplies the trigger, checkout, and credentials:

```yaml
- uses: actions/checkout@v4
- uses: ./.github/actions/sync-datadog-metrics
  with:
    repo: quickwit-oss/quickwit
    lookback: 30d
  env:
    GITHUB_TOKEN: ${{ secrets.METRICS_GITHUB_TOKEN }}
    DD_API_KEY: ${{ secrets.DD_API_KEY }}
```

The action builds the CLI and runs `scripts/sync-datadog-metrics.sh`; it does not define or assume a schedule.
