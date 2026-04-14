/// High-level workflow commands that orchestrate multiple sub-commands,
/// replacing fetch_all_slow.sh, generate_all.sh, and purge_all.sh.
use crate::commands::{build_db, fetch_discussions, fetch_issues, generate_summaries, purge};
use crate::config::Config;
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

/// Ports fetch_all_slow.sh: fetches issues and discussions for each env file.
pub fn fetch_all(env_files: &[String]) -> Result<()> {
    let client = Client::new();
    for env_file in env_files {
        println!("\n=== Fetching for env: {env_file} ===");
        let config = Config::load(Some(env_file))?;
        fetch_issues::run_with_client(&client, &config)?;
        fetch_discussions::run_with_client(&client, &config)?;
    }
    Ok(())
}

/// Ports generate_all.sh: build-db + generate-summaries for each env/input pair.
/// plot.py is kept in Python — this workflow prints the exact commands to run it.
pub fn generate_all(env_files: &[String], exclude_labels: Option<&str>, start: Option<&str>) -> Result<()> {
    for env_file in env_files {
        println!("\n=== Generating for env: {env_file} ===");
        let config = Config::load(Some(env_file))?;
        let input = format!("data/{}_{}_issues.json", config.repo_owner, config.repo_name);
        let db = format!("out/db/{}_{}.db", config.repo_owner, config.repo_name);

        println!("Running with input: {input}");
        build_db::run(&input, &config)?;
        generate_summaries::run(&db, &config, exclude_labels)?;
    }

    // Compute default: first day of current month minus 12 months (matches generate_all.sh)
    let default_start = {
        let now = Utc::now();
        let total_months = now.year() * 12 + now.month() as i32 - 13;
        format!("{}-{:02}", total_months / 12, total_months % 12 + 1)
    };
    let start_arg = start.unwrap_or(&default_start);

    println!("\nTo regenerate charts, run plot.py for each repo:");
    for env_file in env_files {
        let mut cmd = format!(
            "python -m scripts.util.plot --env-file {env_file} --input-dir out/summaries --start {start_arg}"
        );
        if let Some(labels) = exclude_labels {
            cmd.push_str(&format!(" --exclude-labels \"{labels}\""));
        }
        println!("  {cmd}");
    }
    Ok(())
}

/// Ports purge_all.sh: runs all three purge variants.
pub fn purge_all(env_file: &str, older_than: u32, dry_run: bool, yes: bool) -> Result<()> {
    let client = Client::new();
    let config = Config::load(Some(env_file))?;

    println!("\n=== Purge nightly (GitHub + Docker Hub) ===");
    purge::purge_github_versions(
        &client,
        &config.github_token,
        Path::new("out/purge/nightly_github.jsonl"),
        older_than,
        dry_run,
        yes,
        |t| t.contains("nightly"),
    )?;
    purge::purge_dockerhub_images(
        &client,
        &dockerhub_nightly_repo(),
        &config.docker_username()?,
        &config.docker_password()?,
        Path::new("out/purge/nightly_dockerhub.jsonl"),
        30,
        dry_run,
        yes,
        |t| t.starts_with("nightly"),
    )?;

    println!("\n=== Purge untagged (GitHub) ===");
    purge::purge_github_untagged_versions(
        &client,
        &config.github_token,
        Path::new("out/purge/untagged_github.jsonl"),
        older_than,
        dry_run,
        yes,
    )?;

    println!("\n=== Purge vector-dev (Docker Hub) ===");
    purge::purge_dockerhub_images(
        &client,
        &dockerhub_vector_dev_repo(),
        &config.docker_username()?,
        &config.docker_password()?,
        Path::new("out/purge/vector_dev_dockerhub.jsonl"),
        older_than,
        dry_run,
        yes,
        |_| true,
    )?;

    println!("\nAll purge operations completed.");
    Ok(())
}
