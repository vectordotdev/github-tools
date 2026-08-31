/// High-level workflow commands that orchestrate multiple sub-commands,
/// replacing fetch_all_slow.sh, generate_all.sh, and purge_all.sh.
use crate::commands::{
    build_db, fetch_discussions, fetch_issues, generate_charts, generate_summaries, purge,
    push_metrics,
};
use crate::config::{Config, Repo};
use anyhow::Result;
use chrono::{Datelike, Utc};
use reqwest::blocking::Client;
use std::path::Path;

/// Docker Hub repos configurable via env vars with sensible defaults.
fn dockerhub_nightly_repo() -> String {
    std::env::var("DOCKERHUB_NIGHTLY_REPO").unwrap_or_else(|_| "timberio/vector".to_string())
}
fn dockerhub_vector_dev_repo() -> String {
    std::env::var("DOCKERHUB_VECTOR_DEV_REPO").unwrap_or_else(|_| "timberio/vector-dev".to_string())
}

/// Fetches issues and discussions for a single repo.
pub fn fetch_all(repo_str: &str, since: Option<&str>) -> Result<()> {
    let repo = Repo::parse(repo_str)?;
    let config = Config::for_repo(&repo);
    let client = Client::new();
    fetch_issues::run_with_client(&client, &config, since)?;
    fetch_discussions::run_with_client(&client, &config, since)?;
    Ok(())
}

fn fetch_all_with_config(config: &Config, since: Option<&str>) -> Result<()> {
    let client = Client::new();
    fetch_issues::run_with_client(&client, config, since)?;
    fetch_discussions::run_with_client(&client, config, since)?;
    Ok(())
}

/// Fetches recent changes, merges them into `data/`, rebuilds the local database,
/// and submits a current metric snapshot to Datadog.
pub fn sync_metrics(
    config: &Config,
    lookback: Option<&str>,
    dd_api_key: Option<&str>,
    dd_site: Option<&str>,
    prefix: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    println!("=== Fetching {}/{} ===", config.org, config.repo);
    fetch_all_with_config(config, lookback)?;

    println!("\n=== Rebuilding local database ===");
    build_repo_db(config)?;

    println!("\n=== Publishing Datadog snapshot ===");
    push_metrics::run(
        config,
        dd_api_key,
        dd_site,
        lookback,
        prefix,
        dry_run,
    )
}

/// Builds the SQLite database used by summaries and Datadog from committed snapshots.
fn build_repo_db(config: &Config) -> Result<String> {
    let repo_prefix = format!("{}_{}", config.org, config.repo);
    let issues_dir = format!("data/{repo_prefix}/issues");
    let single_file = format!("data/{repo_prefix}_issues.json");
    let db = format!("out/db/{repo_prefix}.db");

    let input = if Path::new(&issues_dir).is_dir() {
        issues_dir.as_str()
    } else {
        single_file.as_str()
    };
    build_db::run(input, config)?;

    let discussions_dir = format!("data/{repo_prefix}/discussions");
    let discussions_file = format!("data/{repo_prefix}_discussions.json");
    let discussions_input = if Path::new(&discussions_dir).is_dir() {
        Some(discussions_dir.as_str())
    } else if Path::new(&discussions_file).exists() {
        Some(discussions_file.as_str())
    } else {
        None
    };
    if let Some(input) = discussions_input {
        let conn = rusqlite::Connection::open(&db)?;
        build_db::load_discussions_from_path(&conn, input)?;
    }

    Ok(db)
}

/// Builds DB + summaries + HTML charts for a single repo.
pub fn generate_all(repo_str: &str, start: Option<&str>) -> Result<()> {
    let repo = Repo::parse(repo_str)?;
    let config = Config::for_repo(&repo);
    let db = build_repo_db(&config)?;

    generate_summaries::run(&db, &config)?;

    let default_start = {
        let now = Utc::now();
        let total_months = now.year() * 12 + now.month() as i32 - 13;
        format!("{}-{:02}", total_months / 12, total_months % 12 + 1)
    };
    let start_arg = start.unwrap_or(&default_start);
    generate_charts::run("out/summaries", repo_str, "docs", Some(start_arg))?;
    Ok(())
}

/// Ports purge_all.sh: runs all three purge variants.
pub fn purge_all(older_than: u32, dry_run: bool, yes: bool) -> Result<()> {
    use anyhow::Context;
    let client = Client::new();
    let github_token = std::env::var("GITHUB_TOKEN").context("GITHUB_TOKEN not set")?;
    let docker_username = std::env::var("DOCKER_USERNAME").context("DOCKER_USERNAME not set")?;
    let docker_password = std::env::var("DOCKER_PASSWORD").context("DOCKER_PASSWORD not set")?;

    println!("\n=== Purge nightly (GitHub + Docker Hub) ===");
    purge::purge_github_versions(
        &client,
        &github_token,
        Path::new("out/purge/nightly_github.jsonl"),
        older_than,
        dry_run,
        yes,
        |t| t.contains("nightly"),
    )?;
    purge::purge_dockerhub_images(
        &client,
        &dockerhub_nightly_repo(),
        &docker_username,
        &docker_password,
        Path::new("out/purge/nightly_dockerhub.jsonl"),
        30,
        dry_run,
        yes,
        |t| t.starts_with("nightly"),
    )?;

    println!("\n=== Purge untagged (GitHub) ===");
    purge::purge_github_untagged_versions(
        &client,
        &github_token,
        Path::new("out/purge/untagged_github.jsonl"),
        older_than,
        dry_run,
        yes,
    )?;

    println!("\n=== Purge vector-dev (Docker Hub) ===");
    purge::purge_dockerhub_images(
        &client,
        &dockerhub_vector_dev_repo(),
        &docker_username,
        &docker_password,
        Path::new("out/purge/vector_dev_dockerhub.jsonl"),
        older_than,
        dry_run,
        yes,
        |_| true,
    )?;

    println!("\nAll purge operations completed.");
    Ok(())
}
