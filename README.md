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
./run-all.sh
```

## Trends

### Issues
![Monthly Issues](images/issues.monthly_issues_trend.png)
![Top Issue Labels](images/issues.top_labels.png)
![Issue Label Counts](images/issues.label_counts.png)
![Top 5 Integration Issue Labels](images/issues.integrations.top_5.monthly_trend.png)
![Top Integration Issue By Label Total Count](images/issues.open_closed_total_label_count.png)

### Pull Requests
![Monthly PRs](images/pull_requests.monthly_issues_trend.png)
![Top PR Labels](images/pull_requests.top_labels.png)
![PR Label Counts](images/pull_requests.label_counts.png)
![Top 5 Integration PR Labels](images/pull_requests.integrations.top_5.monthly_trend.png)
