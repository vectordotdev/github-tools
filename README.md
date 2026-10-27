# github-tools

Data and automation for maintaining open source projects. We use it to understand how Vector, VRL and Quickwit are
doing (where the backlog is growing, which integrations draw the most issues, and how the contributor base changes over
time) and to automate the repetitive parts of running them.

It pulls issues, pull requests and discussions from the GitHub API into SQLite, and from there:

- Publishes interactive **trend dashboards** to GitHub Pages: monthly issue, PR and discussion trends, label and
  integration breakdowns, and contributor activity.
- Submits **project-health metrics** to Datadog (open backlog by age, closure rates) for long-term tracking.
- Measures how contributors react to **AI review bot** comments.
- Runs **maintenance jobs**: closing abandoned PRs, deleting stale branches, and purging old container images.

# Trends

Interactive dashboards, refreshed from committed data snapshots:

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
  fetch-issue-interest  Snapshot open-issue reactions and distinct commenters
  fetch-labels       Fetch all labels for a repository

Pipeline:
  sync-metrics       Fetch GitHub data locally, rebuild DB, and submit Datadog metrics
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
  close-old-prs          Close PRs with 'meta:awaiting author' older than 6 months
  delete-stale-branches  Delete branches with no commits in 4 years
  remove-legacy-label    Remove legacy type labels from issues/PRs
  replace-label          Replace a label on issues, PRs and discussions (preview by default)
```

Run `github-tools <COMMAND> --help` for full argument details.

## Replace label assignments

Use `replace-label` to rename a label or migrate its assignments into another
label in the same repository. If the destination name does not exist, the
command previews a rename that preserves the label's ID, color, description and
assignments. If the destination exists, it previews a merge of assignments. The
separate `remove-legacy-label` command continues to handle migration of legacy
type labels into the issue type field.

Preview all affected issues, pull requests and discussions, including closed and
merged items:

```shell
github-tools replace-label --repo owner/repo \
  --old-label 'old label' --new-label 'replacement label'
```

Add `--apply` to collect a fresh plan, display it, and ask for confirmation.
`--apply --yes` skips the prompt. `--dry-run` explicitly selects the default
preview behavior and cannot be combined with `--apply`.

The command fully paginates the inventory before making changes. For a merge, it
adds and verifies the destination label across the plan first, then rechecks
each item before removing only the source label. Unrelated labels are preserved.
Issues and PRs are queried by label; discussions and their labels are scanned
because the discussion API has no label filter. If discussions are disabled, the
command reports that their assignments cannot be inspected or migrated.

For a rename, the command rechecks both names after confirmation, updates only
the existing label's name, and verifies that the new name resolves to the same
ID. It aborts if the source changed or the destination appeared since the
preview. If a rename has an ambiguous outcome, inspect the names before
retrying: a completed rename means the old name no longer exists.

Any API, permission or verification failure stops the run with a nonzero exit.
For a merge, an addition-phase failure leaves every source assignment untouched
by this command; a removal-phase failure preserves earlier completed
replacements. Rerun the same command to reconcile current state: items with both
labels only need the source removed, and completed items are absent from the
next plan. Ambiguous requests are not automatically retried or rolled back.

After a merge, a final inventory reports remaining source assignments, including
those added during the run. GitHub does not provide an atomic label swap, so
concurrent edits can still race with verification; run during a quiet period and
update any automation that applies the old label. A merge retains both
repository label definitions. The command uses `GITHUB_TOKEN` / `--env-file`
when provided, otherwise it uses `gh api` with its existing authentication
without extracting its token. Merging requires label-editing permission on every
planned item; renaming requires permission to edit repository labels.

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
All saved JSON uses alphabetical object keys, including nested objects, so response key order cannot create diffs. Array order is unchanged.

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

Measures thumbs-up/down feedback on review-thread comments on **merged PRs**.
Top-level review summaries, issue comments, and open/unmerged PRs are excluded.

```shell
# Discover author logins without saving data
op run --env-file secrets.env -- github-tools automated-review-stats \
  --repo vectordotdev/vector --since 3m

# Initial collection, or widen saved coverage with an earlier cutoff
op run --env-file secrets.env -- github-tools automated-review-stats \
  --repo vectordotdev/vector --bot-login chatgpt-codex-connector --since 2026-01-01

# Subsequent incremental refreshes (reuse each bot's saved coverage)
op run --env-file secrets.env -- github-tools automated-review-stats \
  --repo vectordotdev/vector --bot-login chatgpt-codex-connector
op run --env-file secrets.env -- github-tools automated-review-stats \
  --repo vectordotdev/vector --bot-login datadoghq-integration

# Periodic full reconciliation of discovery, within saved merged-date coverage
op run --env-file secrets.env -- github-tools automated-review-stats \
  --repo vectordotdev/vector --bot-login chatgpt-codex-connector --full-refresh

# Rebuild summaries and reports offline from tracked snapshots
github-tools generate-all --repo vectordotdev/vector
```

Source records live in `data/{owner}_{repo}/automated-review-stats/{bot_login}/{year}.json`,
bucketed by **PR merge year**. Each PR stores its number and merge timestamp once,
with a list of minimal comment records (`id`, `thumbs_up`, `thumbs_down`). Comment
URLs and timestamps are omitted; PR links are derived when generating summaries.
Node IDs retain efficient batched reaction refreshes. Each bot has a
`state.json` with its merged-date coverage, successful discovery watermark,
collection time, and PR count for the latest pass. Codex and Bits never overwrite
each other. The initial Vector snapshots were collected on October 19, 2026:
1,926 Codex comments and 21 Bits comments on PRs merged since January 1, 2026.

Incremental discovery scans merged PR metadata ordered by update time, stopping
at the previous successful run's **start** minus a one-day overlap. Only matching
PRs have their threads/comments fetched, with full pagination. Every saved comment
also gets a batched reaction-count refresh, because reaction changes need not bump
PR update timestamps. Counts use GraphQL totals, not a page of individual reactions.
Missing nodes are removed when GitHub returns null without errors; API errors abort
without saving the collected results. No watermark advances until fetching and data
writes succeed. Run one collector per bot/repository at a time.

`--since` defines cumulative merged-date coverage, not an incremental update window.
Omit it on subsequent runs; an earlier date backfills, while narrowing saved coverage
is rejected. Without saved data or `--since`, collection covers all time. Use
`--full-refresh` periodically to reconcile discovery beyond the overlap window.
Snapshots retain the latest comment state, like issues/discussions; they do not
reconstruct when reactions occurred. Git history retains prior committed snapshots.
Mixed thumbs count as `no_signal`, with a separate `mixed` count in summaries.

Generated, gitignored outputs:

- `out/automated-review-stats/{owner}_{repo}_{bot_login}.csv`: one row per PR, with
  one PR link, comment classifications, and summed thumbs-up/down reaction counts.
- `out/summaries/{owner}_{repo}_automated_review_stats.json`: all saved bot summaries,
  including per-PR aggregates in each bot's `pull_requests` array.

Comment classifications and reaction totals remain distinct: a comment with ten
thumbs-up reactions counts as one liked comment and ten positive reactions. The
charts continue to show comment classifications. Storage schema version 2 groups
records by PR; the snapshots in this branch have been converted from version 1.

`generate-all` rebuilds these outputs from tracked data and renders one AI chart per
bot. The collection command does not edit `trends/*.md`; HTML reports are generated
separately. Neither command commits or pushes data. This collector is not scheduled
by the Datadog metrics workflow.

## 5. Datadog project-health metrics

`sync-metrics` is the end-to-end path intended for scheduled automation:

```shell
op run --env-file secrets.env -- github-tools sync-metrics \
  --repo vectordotdev/vector \
  --lookback 7d \
  --activity-window 30d
```

The command:

1. Fetches a complete GitHub snapshot into the runner-local `data/{owner}_{repo}/` directory.
2. Rebuilds `out/db/{owner}_{repo}.db` from that local snapshot.
3. Reconstructs one snapshot at each UTC midnight in `--lookback`, adds the current snapshot, and sends them to Datadog. `--activity-window` independently controls the rolling period summarized by the closed-item metrics.

The fetched JSON and SQLite database are temporary automation inputs. The command never stages, commits, or pushes them.

Use `--dry-run` to perform the fetch and database rebuild while printing, but not submitting, the resulting metrics. To preview metrics entirely offline after a database has been built:

```shell
github-tools push-metrics \
  --repo vectordotdev/vector \
  --history 30d \
  --activity-window 30d \
  --dry-run
```

Use `--output-json` when a Datadog workflow or agent owns the Datadog connection. It performs the same calculation without submitting and emits one final JSON envelope. Each object in `batches` is a size-safe request body that can be sent directly to `POST /api/v2/series`:

```shell
github-tools sync-metrics \
  --repo quickwit-oss/quickwit \
  --lookback 7d \
  --activity-window 30d \
  --output-json
```

```json
{"format":"datadog-series-batches-v1","series_count":1,"point_count":1,"batches":[{"series":[{"metric":"github.health.v2.issues","type":3,"points":[{"timestamp":1788278400,"value":42}],"tags":["repo:quickwit-oss/quickwit"]}]}]}
```

The default prefix is `github.health.v2`; override it with `--prefix`. Increment the prefix version before changing the metric dimensions so a clean backfill cannot mix incompatible tag schemas. The metrics are:

| Metric | Meaning | Important tags |
|---|---|---|
| `github.health.v2.issues` | Open issue backlog at each snapshot | `repo`, `issue_type`, `age`, labels |
| `github.health.v2.prs` | Open non-draft PR backlog at each snapshot | `repo`, `age`, labels |
| `github.health.v2.discussions` | Open discussions at each snapshot | `repo`, `category`, `answered`, `age` |
| `github.health.v2.issues.closed` | Issues closed in the rolling window ending at each snapshot | `repo`, `window`, `issue_type`, labels |
| `github.health.v2.prs.closed` | PRs closed in the rolling window ending at each snapshot | `repo`, `window`, labels |
| `github.health.v2.discussions.closed` | Discussions closed in the rolling window ending at each snapshot | `repo`, `window`, `category`, `answered` |

All are gauges. Rolling-window totals remain gauges because each point is a complete window snapshot, not a count accumulated during the reporting interval.

### Historical backfill and weekly updates

Historical Metrics Ingestion must be enabled before submitting the backfill. For a new prefix:

1. Submit a one-day seed using `--lookback 1d --activity-window 30d` so the metric names exist.
2. In Datadog **Metrics Summary**, choose **Configure Metrics → Enable historical metrics** and select the `github.health.v2` namespace.
3. Submit the one-time backfill with `--lookback 450d --activity-window 30d`.
4. Schedule subsequent runs at a fixed UTC time with `--lookback 7d --activity-window 30d`.

Closed metrics are sparse: a metric with no matching closures is not emitted. If a closed metric is absent after the seed, use a wider activity window to create its name before enabling Historical Metrics Ingestion.

The 450-day limit fits within Datadog's default 15-month metric retention. The seven-day weekly lookback intentionally avoids rewriting the preceding week's UTC-midnight snapshots. A failed or skipped weekly run can leave a gap and requires an explicit catch-up run.

Datadog treats points older than one hour as historical and bills them as indexed custom metrics. Older points also have higher ingestion latency; see [Historical Metrics Ingestion](https://docs.datadoghq.com/metrics/custom_metrics/historical_metrics/).

After enabling it, an existing database can be backfilled without fetching GitHub again:

```shell
github-tools push-metrics \
  --repo quickwit-oss/quickwit \
  --history 450d \
  --activity-window 30d
```

Dashboard totals must sum the dimensional series. For example:

```text
sum:github.health.v2.issues{repo:quickwit-oss/quickwit}
sum:github.health.v2.prs{repo:quickwit-oss/quickwit}
sum:github.health.v2.discussions{repo:quickwit-oss/quickwit}
sum:github.health.v2.issues.closed{repo:quickwit-oss/quickwit,window:30d}
sum:github.health.v2.prs.closed{repo:quickwit-oss/quickwit,window:30d}
sum:github.health.v2.discussions.closed{repo:quickwit-oss/quickwit,window:30d}
```

Do not use `avg:` for repository totals: each metric is split into multiple tag combinations for its breakdown dimensions.

An external scheduler invokes `sync-metrics` once per repository. Its GitHub token needs read access to the source repository. Direct submission needs `DD_API_KEY`; alternatively, `--output-json` lets a Datadog workflow submit each generated batch through a managed HTTP connection. Non-US1 accounts should also set `DD_SITE` (for example, `datadoghq.eu`) for direct submission.

### GitHub Actions dispatch

The `Submit repository-health metrics` workflow provides a networked execution path for Datadog Workflow Automation. Configure these GitHub Actions secrets on this repository:

- `DD_API_KEY_DASHBOARD_METRICS`: submits metrics to Datadog.
- `PAT_TOKEN_DASHBOARD_METRICS`: reads issues, pull requests, and discussions from the target repository.

Dispatch the workflow with `repository`, `lookback`, `activity_window`, and `metric_prefix`. The caller supplies no shell command or credentials; the workflow validates those inputs and constructs the fixed `sync-metrics` command.

Tags and series are emitted in deterministic order, and an identical metric name, timestamp, and tag combination is safe to resubmit because Datadog retains the most recently submitted value. Current GitHub snapshots do not contain the full timelines for labels, issue types, PR draft transitions, discussion categories/answers, or close/reopen cycles. Historical breakdowns using those mutable fields are therefore current-state approximations. Use a new prefix version for a clean corrective backfill if the schema changes; exact lifecycle reconstruction would require fetching GitHub timeline events.

### Issue interest snapshots

`fetch-issue-interest --repo owner/name` writes
`data/owner_name/issue-interest/latest.json`. It scans every open issue and all
comment pages, then saves total issue reactions and distinct commenter counts.
Every identifiable commenter counts once, including the author and bots;
comments whose author is unavailable are recorded separately and cannot be
attributed to a distinct account. Comment reactions are not included.

`fetch-all` also refreshes this snapshot. This is a complete open-issue scan even
with `--since`, because reactions may change without changing an issue's update
timestamp. Closed issues disappear from the next complete snapshot. A failed
request leaves the previous snapshot intact. Snapshots contain counts and issue
titles, not comment bodies or commenter identities, and retain canonical key order.
