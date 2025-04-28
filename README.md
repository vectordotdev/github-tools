# github-tools

> [!WARNING]  
> Under Construction. I force push to this frequently.


These are some tools for extracting data for GitHub, storing them in a local db, querying them and finally visualizing them.

# Configuration

For GitHub integrations, you will need a `.env` file. Example:

```dotenv
GITHUB_TOKEN=REDACTED
REPO_OWNER=vectordotdev
REPO_NAME=vector
```

# Run

The following script deletes and regenerates everything.

```shell
./run-all.sh path/to/my.env
```

## Trends

### Issues

![Monthly Issues](out/images/vectordotdev_vector_issues.monthly_issues_trend.png)

---

![Top Issue Labels](out/images/vectordotdev_vector_issues.top_labels.png)

---

![Issue Label Counts](out/images/vectordotdev_vector_issues.label_counts.png)

---

![Top 5 Integration Issue Labels](out/images/vectordotdev_vector_issues.integrations.top_5.monthly_trend.png)

---

![Top Integration Issue By Label Total Count](out/images/vectordotdev_vector_issues.open_closed_total_label_count.png)

### Pull Requests

![Monthly PRs](out/images/vectordotdev_vector_pull_requests.monthly_issues_trend.png)

---

![Top PR Labels](out/images/vectordotdev_vector_pull_requests.top_labels.png)

---

![PR Label Counts](out/images/vectordotdev_vector_pull_requests.label_counts.png)

---

![Top 5 Integration PR Labels](out/images/vectordotdev_vector_pull_requests.integrations.top_5.monthly_trend.png)

---

![Top Integration PRs By Label Total Count](out/images/vectordotdev_vector_pull_requests.open_closed_total_label_count.png)

### Discussions

TODO!
