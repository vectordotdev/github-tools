# github-tools

> [!NOTE]
> This repo is actively developed. Main branch may be rewritten.

Tools for extracting data from GitHub, storing it in a local SQLite database, querying it, and visualizing trends.

# Directory Layout

```
src/             # Rust source (single binary: github-tools)
scripts/util/    # Python: plot.py (charts), json_to_csv.py (utility)
data/            # Committed snapshots: JSON inputs and PNG charts
  {owner}_{repo}/issues/  # Issues/PRs JSON split by year (2024.json, 2025.json, ...)
  images/        # Committed PNG charts (written directly by plot.py)
out/             # Gitignored — all generated and local-only files
  historical/    # Raw JSON fetched from GitHub API
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

Most commands take an `--env-file` pointing to a `.env` file:

```dotenv
GITHUB_TOKEN=...
REPO_OWNER=vectordotdev
REPO_NAME=vector
DOCKER_USERNAME=...   # purge commands only
DOCKER_PASSWORD=...   # purge commands only
```

# Commands

```
github-tools <COMMAND>

Fetch:
  fetch-all          Fetch issues + discussions for all repos (workflow)
  fetch-issues       Fetch all issues/PRs for a repository
  fetch-discussions  Fetch all discussions for a repository
  fetch-labels       Fetch all labels for a repository

Pipeline:
  generate-all       Build DB + summaries for all repos (workflow)
  build-db           Load issues JSON into SQLite database
  generate-summaries Generate CSV summaries from SQLite database

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
github-tools fetch-all --env-file vector.env --env-file vrl.env --env-file quickwit.env
```

Writes to `out/historical/`. The fetched JSON must be split by year and promoted to `data/` to commit as a snapshot. Issues/PRs are stored in `data/{owner}_{repo}/issues/{year}.json`.

## 2. Generate DB, summaries, and charts

```shell
github-tools generate-all \
  --env-file vector.env --env-file vrl.env --env-file quickwit.env

# Charts (still Python). --exclude-labels hides those label series from
# label-frequency charts only; the underlying PR/issue counts are unaffected.
python -m scripts.util.plot --env-file vector.env --input-dir out/summaries \
  --start $(date -d "$(date +%Y-%m-01) -12 months" +%Y-%m) \
  --exclude-labels "no-changelog,meta: awaiting author"
```

Charts are written directly into `data/images/`. Review the diff before committing.

## 3. (Optional) Purge stale container images

```shell
github-tools purge-all --env-file vector.env --dry-run
github-tools purge-all --env-file vector.env  # omit --dry-run to execute
```

Audit logs written to `out/purge/` (local only).

## 4. AI-assisted review stats

Measures how contributors react to automated review bot comments (👍 liked / 👎 disliked / no signal).

```shell
# Discover the bot's GitHub login (lists all review comment authors by frequency)
github-tools automated-review-stats --env-file vector.env --since 3m

# Produce stats + update trends/vector.md
github-tools automated-review-stats \
  --env-file vector.env \
  --bot-login "chatgpt-codex-connector" \
  --since 2026-01-01
```

Outputs:
- Console summary (like rate, dislike rate)
- `out/automated-review-stats/{owner}_{repo}.csv` — per-comment table with URL and reaction (gitignored)
- `trends/{repo}.md` — two summary tables updated in place via `AUTO:` markers

# Trends

Per-repo trend pages with all charts:

- [Vector](trends/vector.md)
- [VRL](trends/vrl.md)
- [Quickwit](trends/quickwit.md)

Exclusions are now per-chart; see the note below each chart on the trends pages.
