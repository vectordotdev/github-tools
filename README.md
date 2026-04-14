# github-tools

> [!NOTE]
> This repo is actively developed. Main branch may be rewritten.

Tools for extracting data from GitHub, storing it in a local SQLite database, querying it, and visualizing trends.

# Directory Layout

```
src/             # Rust source (single binary: github-tools)
scripts/util/    # Python: plot.py (charts), json_to_csv.py (utility)
data/            # Committed snapshots: JSON inputs and PNG charts
  images/        # Committed PNG charts (promoted from out/images/)
out/             # Gitignored — all generated and local-only files
  historical/    # Raw JSON fetched from GitHub API
  db/            # SQLite databases
  summaries/     # Generated CSVs
  images/        # Generated PNG charts (promote to data/images/ to commit)
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

Maintenance:
  close-old-prs          Close PRs with 'meta: awaiting author' older than 6 months
  delete-stale-branches  Delete branches with no commits in 4 years
  remove-legacy-label    Remove legacy type labels from issues/PRs
```

Run `github-tools <COMMAND> --help` for full argument details.

# Workflow

## 1. (Optional) Fetch fresh data from GitHub

```shell
github-tools fetch-all --env-file vector.env --env-file vrl.env
```

Writes to `out/historical/`. Promote to `data/` to commit as a snapshot:

```shell
cp out/historical/issues/vectordotdev_vector_issues.json data/
cp out/historical/issues/vectordotdev_vrl_issues.json data/
cp out/historical/discussions/vectordotdev_vector_discussions.json data/
cp out/historical/discussions/vectordotdev_vrl_discussions.json data/
```

## 2. Generate DB, summaries, and charts

```shell
github-tools generate-all \
  --env-file vector.env --env-file vrl.env \
  --exclude-labels "no-changelog,meta: awaiting author"

# Charts (still Python):
python -m scripts.util.plot --env-file vector.env --input-dir out/summaries \
  --start $(date -d "$(date +%Y-%m-01) -12 months" +%Y-%m) \
  --exclude-labels "no-changelog,meta: awaiting author"
```

Promote charts to commit:

```shell
cp out/images/*.png data/images/
```

## 3. (Optional) Purge stale container images

```shell
github-tools purge-all --env-file vector.env --dry-run
github-tools purge-all --env-file vector.env  # omit --dry-run to execute
```

Audit logs written to `out/purge/` (local only).

# Trends

> [!NOTE]
> Draft PRs are excluded. Issues and PRs with the following labels are excluded from all charts: `no-changelog`, `meta: awaiting author`.

#### Vector

##### Issues

![Monthly Issues](data/images/vectordotdev_vector_issues.monthly_trend.png)

---

![Top Issue Labels](data/images/vectordotdev_vector_issues.top_labels.png)

---

![Issue Label Counts](data/images/vectordotdev_vector_issues.label_counts.png)

---

![Top 5 Integration Issue Labels](data/images/vectordotdev_vector_issues.integrations.top_5.monthly_trend.png)

---

![Top Integration Issue By Label Total Count](data/images/vectordotdev_vector_issues.open_closed_total_label_count.png)

##### Pull Requests

![Monthly PRs](data/images/vectordotdev_vector_pull_requests.monthly_trend.png)

---

![Top PR Labels](data/images/vectordotdev_vector_pull_requests.top_labels.png)

---

![PR Label Counts](data/images/vectordotdev_vector_pull_requests.label_counts.png)

---

![Top 5 Integration PR Labels](data/images/vectordotdev_vector_pull_requests.integrations.top_5.monthly_trend.png)

---

![Top Integration PRs By Label Total Count](data/images/vectordotdev_vector_pull_requests.open_closed_total_label_count.png)

##### Discussions

![Monthly Discussions](data/images/vectordotdev_vector_discussions.monthly_trend.png)

---

#### VRL

##### Issues

![Monthly Issues](data/images/vectordotdev_vrl_issues.monthly_trend.png)

---

![Top Issue Labels](data/images/vectordotdev_vrl_issues.top_labels.png)

---

![Issue Label Counts](data/images/vectordotdev_vrl_issues.label_counts.png)

##### Pull Requests

![Monthly PRs](data/images/vectordotdev_vrl_pull_requests.monthly_trend.png)

---

![Top PR Labels](data/images/vectordotdev_vrl_pull_requests.top_labels.png)

---

![PR Label Counts](data/images/vectordotdev_vrl_pull_requests.label_counts.png)

##### Discussions

![Monthly Discussions](data/images/vectordotdev_vrl_discussions.monthly_trend.png)
