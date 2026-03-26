# github-tools

> [!WARNING]
> Under Construction. I force push to this frequently.

Tools for extracting data from GitHub, storing it in a local SQLite database, querying it, and visualizing trends.

# Directory Layout

```
data/        # Committed JSON snapshots used as pipeline input (issues, discussions)
out/         # Gitignored — all generated and local-only files
  historical/  # Raw JSON fetched from GitHub API (fetch scripts write here)
  db/          # SQLite databases
  summaries/   # Generated CSVs
  images/      # Generated PNG charts
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

After fetching, promote the files to `data/` to commit them as a new snapshot:

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

Charts are written to `out/images/`.

## 3. (Optional) Purge stale container images

```shell
./purge_all.sh
```

Audit logs are written to `out/purge/` (local only, not committed).
