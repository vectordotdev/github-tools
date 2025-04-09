# github-tools

 > [!WARNING]  
> Under Construction. I force push to this frequently.

These are some tools for extracting data for github and saving them for further analysis.

# Configuration

For GitHub integrations, you will need a `.env` file. Example:

```dotenv
GITHUB_TOKEN=REDACTED
REPO_OWNER=vectordotdev
REPO_NAME=vector
```

# Previous Results

Look at the `static` directory. I manually saved some snapshots in there.

# Run

```shell
python /Users/pavlos.rontidis/PycharmProjects/GithubTools/scripts/db/sqlite_writer.py --input /Users/pavlos.rontidis/PycharmProjects/GithubTools/static/20250417_143706_vectordotdev_vector_issues.json 
python scripts/db/generate_summary.py --db static/20250418_095233_vectordotdev_vector_issues.db
python scripts/util/plot.py --start 2024-06 --monthly out/monthly_summary.csv --labels out/label_breakdown.csv --label_counts out/label_timeseries.csv --exclude-labels no-changelog,domain: deps 
```
