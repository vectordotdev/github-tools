# github-tools

> [!NOTE]
> This repo is actively developed. Main branch may be rewritten.

Tools for extracting data from GitHub, storing it in a local SQLite database, querying it, and visualizing trends.

# Directory Layout

```
data/        # Committed snapshots: JSON inputs and PNG charts
  images/    # Committed PNG charts (promoted from out/images/)
out/         # Gitignored — all generated and local-only files
  historical/  # Raw JSON fetched from GitHub API (fetch scripts write here)
  db/          # SQLite databases
  summaries/   # Generated CSVs
  images/      # Generated PNG charts (promote to data/images/ to commit)
  purge/       # Purge audit logs (local only)
scripts/     # All Python logic
```

# Configuration

For GitHub integrations, you will need a `.env` file. Example:

```dotenv
GITHUB_TOKEN=REDACTED
REPO_OWNER=vectordotdev
REPO_NAME=vector
```

# Workflow

## 1. (Optional) Fetch fresh data from GitHub

Fetches all issues, PRs, and discussions into `out/historical/`. This is slow.

```shell
./fetch_all_slow.sh
```

After fetching, promote the JSON files to `data/` to commit them as a new snapshot:

```shell
cp out/historical/issues/vectordotdev_vector_issues.json data/
cp out/historical/issues/vectordotdev_vrl_issues.json data/
cp out/historical/discussions/vectordotdev_vector_discussions.json data/
cp out/historical/discussions/vectordotdev_vrl_discussions.json data/
```

## 2. Generate DB, summaries, and charts

Reads from `data/`, writes all output to `out/`.

```shell
./generate_all.sh
```

After generating, promote the charts to `data/images/` to commit them:

```shell
cp out/images/*.png data/images/
```

## 3. (Optional) Purge stale container images

```shell
./purge_all.sh
```

Audit logs are written to `out/purge/` (local only, not committed).

# Trends

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

TODO!

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
