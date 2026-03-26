use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::Value;
use std::fs;
use std::path::Path;

const BATCH_SIZE: u32 = 100;

pub fn run(config: &Config) -> Result<()> {
    run_with_client(&Client::new(), config)
}

pub fn run_with_client(client: &Client, config: &Config) -> Result<()> {
    let issues = fetch_all_issues(client, config)?;

    println!("Total issues/PRs fetched: {}", issues.len());

    let out_dir = Path::new("out/historical/issues");
    fs::create_dir_all(out_dir)?;
    let out_file = out_dir.join(format!("{}_{}_issues.json", config.repo_owner, config.repo_name));
    let json = serde_json::to_string_pretty(&issues)?;
    fs::write(&out_file, json)?;
    println!("Saved to '{}'", out_file.display());

    Ok(())
}

fn fetch_all_issues(client: &Client, config: &Config) -> Result<Vec<Value>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/issues",
        config.repo_owner, config.repo_name
    );

    let mut issues = Vec::new();
    let mut page = 1u32;

    loop {
        println!("Fetching page {page} (batch size: {BATCH_SIZE}, state: all)...");

        let response = client
            .get(&url)
            .header("Authorization", format!("token {}", config.github_token))
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "github-tools")
            .query(&[
                ("state", "all"),
                ("per_page", &BATCH_SIZE.to_string()),
                ("page", &page.to_string()),
            ])
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            eprintln!("Warning: API request failed on page {page} ({status}): {body}");
            break;
        }

        let page_items: Vec<Value> = response.json().context("Failed to parse response")?;
        let done = page_items.is_empty() || page_items.len() < BATCH_SIZE as usize;

        println!("Page {page} fetched. Total collected: {}", issues.len() + page_items.len());
        issues.extend(page_items);

        if done {
            println!("Reached the last page.");
            break;
        }
        page += 1;
    }

    Ok(issues)
}
