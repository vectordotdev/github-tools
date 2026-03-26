use crate::config::Config;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use reqwest::blocking::Client;
use serde_json::Value;

const AWAITING_LABEL: &str = "meta: awaiting author";
const CLOSE_COMMENT: &str = "\
Thank you for your contribution to Vector! To keep the repository tidy and focused, \
we are closing this PR due to inactivity. \
We greatly appreciate the time and effort you've put into this PR. \
If you'd like to continue working on it, we encourage you to re-open the PR and we would be delighted to review it again. \
Before re-opening, please git merge origin master to resolve any conflicts with origin/master.";

pub fn run(config: &Config, dry_run: bool, yes: bool) -> Result<()> {
    let client = Client::new();
    let cutoff = Utc::now() - Duration::days(6 * 30);

    println!("Fetching open PRs older than {}...", cutoff.date_naive());
    let prs = fetch_open_prs(&client, config)?;
    println!("Total open PRs fetched: {}", prs.len());

    // Collect matching PRs before acting
    let mut matched: Vec<(u64, String, DateTime<Utc>)> = Vec::new();

    for pr in &prs {
        let number = pr["number"].as_u64().unwrap_or(0);
        let title = pr["title"].as_str().unwrap_or("");
        let created_at = pr["created_at"].as_str().unwrap_or("");
        let created_dt = created_at.parse::<DateTime<Utc>>().unwrap_or(Utc::now());

        if created_dt >= cutoff {
            continue;
        }

        let labels: Vec<&str> = pr["labels"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| l["name"].as_str()).collect())
            .unwrap_or_default();

        if labels.contains(&AWAITING_LABEL) {
            matched.push((number, title.to_string(), created_dt));
        }
    }

    if matched.is_empty() {
        println!("No matching PRs found.");
        return Ok(());
    }

    println!("\nFound {} PR(s) to close:", matched.len());
    for (number, title, created_at) in &matched {
        println!(
            "  PR #{number}: {title} (created: {})",
            created_at.date_naive()
        );
    }

    if dry_run {
        println!("\n[dry-run] No changes made.");
        return Ok(());
    }

    if !crate::confirm(&format!("Close {} PR(s)?", matched.len()), yes) {
        println!("Aborted.");
        return Ok(());
    }

    for (number, _, _) in &matched {
        add_comment(&client, config, *number, CLOSE_COMMENT)?;
        close_pr(&client, config, *number)?;
    }

    println!("\nClosed {} PR(s).", matched.len());
    Ok(())
}

fn fetch_open_prs(client: &Client, config: &Config) -> Result<Vec<Value>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pulls",
        config.repo_owner, config.repo_name
    );
    let mut prs = Vec::new();
    let mut page = 1u32;

    loop {
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", config.github_token))
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "github-tools")
            .query(&[
                ("state", "open"),
                ("per_page", "100"),
                ("page", &page.to_string()),
            ])
            .send()
            .context("Failed to fetch PRs")?
            .error_for_status()?;

        let batch: Vec<Value> = resp.json()?;
        let done = batch.len() < 100;
        prs.extend(batch);
        if done {
            break;
        }
        page += 1;
    }
    Ok(prs)
}

fn add_comment(client: &Client, config: &Config, pr_number: u64, body: &str) -> Result<()> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/issues/{pr_number}/comments",
        config.repo_owner, config.repo_name
    );
    client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.github_token))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "github-tools")
        .json(&serde_json::json!({"body": body}))
        .send()?
        .error_for_status()
        .with_context(|| format!("Failed to add comment to PR #{pr_number}"))?;
    println!("Added comment to PR #{pr_number}");
    Ok(())
}

fn close_pr(client: &Client, config: &Config, pr_number: u64) -> Result<()> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pulls/{pr_number}",
        config.repo_owner, config.repo_name
    );
    client
        .patch(&url)
        .header("Authorization", format!("Bearer {}", config.github_token))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "github-tools")
        .json(&serde_json::json!({"state": "closed"}))
        .send()?
        .error_for_status()
        .with_context(|| format!("Failed to close PR #{pr_number}"))?;
    println!("Closed PR #{pr_number}");
    Ok(())
}
