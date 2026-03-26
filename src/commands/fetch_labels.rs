use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Deserialize, Serialize)]
pub struct Label {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

pub fn run(config: &Config) -> Result<()> {
    let client = Client::new();
    let labels = fetch_all_labels(&client, config)?;

    println!("Total labels found: {}", labels.len());
    for label in &labels {
        println!(
            "- Name: {}, Color: #{}, Description: {}",
            label.name,
            label.color,
            label.description.as_deref().unwrap_or("No description")
        );
    }

    let out_dir = Path::new("out/labels");
    fs::create_dir_all(out_dir)?;
    let out_file = out_dir.join(format!("{}_{}_labels.json", config.repo_owner, config.repo_name));
    let json = serde_json::to_string_pretty(&labels)?;
    fs::write(&out_file, json)?;
    println!("Labels saved to '{}'", out_file.display());

    Ok(())
}

fn fetch_all_labels(client: &Client, config: &Config) -> Result<Vec<Label>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/labels",
        config.repo_owner, config.repo_name
    );

    let mut labels = Vec::new();
    let mut page = 1u32;
    const PER_PAGE: u32 = 100;

    loop {
        println!("Fetching page {page} of labels...");

        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", config.github_token))
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "github-tools")
            .query(&[("per_page", PER_PAGE), ("page", page)])
            .send()
            .context("Failed to send request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            anyhow::bail!("API request failed: {status} - {body}");
        }

        let page_labels: Vec<Label> = response.json().context("Failed to parse labels response")?;
        let done = page_labels.len() < PER_PAGE as usize;
        labels.extend(page_labels);

        if done {
            break;
        }
        page += 1;
    }

    Ok(labels)
}
