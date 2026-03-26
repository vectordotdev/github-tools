use crate::config::Config;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use reqwest::blocking::Client;
use serde_json::Value;

const STALE_YEARS: i64 = 4;

pub fn run(config: &Config) -> Result<()> {
    let client = Client::new();
    let cutoff = Utc::now() - Duration::days(STALE_YEARS * 365);

    println!("Fetching branches for {}/{}...", config.repo_owner, config.repo_name);
    let branches = fetch_all_branches(&client, config)?;
    println!("Total branches: {}", branches.len());

    for branch in &branches {
        let name = branch["name"].as_str().unwrap_or("");
        let protected = branch["protected"].as_bool().unwrap_or(false);

        if protected || name == "main" || name == "master" || is_semver_branch(name) {
            println!("Skipping special branch: {name}");
            continue;
        }

        match get_last_commit_date(&client, config, name)? {
            Some(last_commit) if last_commit > cutoff => {
                println!("Keeping active branch: {name} (last commit: {})", last_commit.date_naive());
            }
            Some(last_commit) => {
                println!("Deleting stale branch: {name} (last commit: {})", last_commit.date_naive());
                delete_branch(&client, config, name)?;
            }
            None => {
                println!("Could not determine activity for branch: {name}");
            }
        }
    }

    Ok(())
}

fn is_semver_branch(name: &str) -> bool {
    let stripped = name.strip_prefix('v').unwrap_or(name);
    semver::Version::parse(stripped).is_ok()
}

fn fetch_all_branches(client: &Client, config: &Config) -> Result<Vec<Value>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/branches",
        config.repo_owner, config.repo_name
    );
    let mut branches = Vec::new();
    let mut page = 1u32;

    loop {
        let resp = client
            .get(&url)
            .header("Authorization", format!("token {}", config.github_token))
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "github-tools")
            .query(&[("per_page", "100"), ("page", &page.to_string())])
            .send()
            .context("Failed to fetch branches")?
            .error_for_status()?;

        let has_next = resp
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .map(|l| l.contains("rel=\"next\""))
            .unwrap_or(false);

        let batch: Vec<Value> = resp.json()?;
        if batch.is_empty() { break; }
        branches.extend(batch);
        if !has_next { break; }
        page += 1;
    }
    Ok(branches)
}

fn get_last_commit_date(client: &Client, config: &Config, branch: &str) -> Result<Option<DateTime<Utc>>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/commits",
        config.repo_owner, config.repo_name
    );
    let resp = client
        .get(&url)
        .header("Authorization", format!("token {}", config.github_token))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "github-tools")
        .query(&[("sha", branch), ("per_page", "1")])
        .send()?
        .error_for_status()?;

    let commits: Vec<Value> = resp.json()?;
    let date_str = commits
        .first()
        .and_then(|c| c["commit"]["committer"]["date"].as_str());

    Ok(date_str.and_then(|s| s.parse::<DateTime<Utc>>().ok()))
}

fn delete_branch(client: &Client, config: &Config, branch: &str) -> Result<()> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/git/refs/heads/{}",
        config.repo_owner, config.repo_name, branch
    );
    let resp = client
        .delete(&url)
        .header("Authorization", format!("token {}", config.github_token))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "github-tools")
        .send()?;

    if resp.status().as_u16() == 204 {
        println!("Deleted branch: {branch}");
    } else {
        eprintln!("Failed to delete branch {branch}: {} - {}", resp.status(), resp.text()?);
    }
    Ok(())
}
