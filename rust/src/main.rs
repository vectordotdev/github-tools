use anyhow::Result;
use clap::{Parser, Subcommand};
use github_tools::{commands::{build_db, fetch_discussions, fetch_issues, fetch_labels, generate_summaries, purge}, config::Config};
use reqwest::blocking::Client;
use std::path::Path;

#[derive(Parser)]
#[command(name = "github-tools", about = "GitHub data extraction and analysis tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch all labels for a repository
    FetchLabels {
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
    },
    /// Fetch all issues and PRs for a repository
    FetchIssues {
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
    },
    /// Fetch all discussions for a repository
    FetchDiscussions {
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
    },
    /// Load issues JSON into SQLite database
    BuildDb {
        #[arg(long, help = "Path to issues JSON file")]
        input: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
    },
    /// Generate CSV summaries from SQLite database
    GenerateSummaries {
        #[arg(long, help = "Path to SQLite database")]
        db: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
        #[arg(long, help = "Comma-separated labels to exclude")]
        exclude_labels: Option<String>,
    },
    /// Purge stale container images
    Purge {
        #[command(subcommand)]
        target: PurgeTarget,
    },
}

#[derive(Subcommand)]
enum PurgeTarget {
    /// Purge old nightly images from GitHub and/or Docker Hub
    Nightly {
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long, help = "Path to .env file")]
        env_file: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Purge untagged GitHub container images
    Untagged {
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long, help = "Path to .env file")]
        env_file: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Purge old vector-dev images from Docker Hub
    VectorDev {
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long, help = "Path to .env file")]
        env_file: String,
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::FetchLabels { env_file } => {
            let config = Config::load(env_file.as_deref())?;
            fetch_labels::run(&config)
        }
        Command::FetchIssues { env_file } => {
            let config = Config::load(env_file.as_deref())?;
            fetch_issues::run(&config)
        }
        Command::FetchDiscussions { env_file } => {
            let config = Config::load(env_file.as_deref())?;
            fetch_discussions::run(&config)
        }
        Command::BuildDb { input, env_file } => {
            let config = Config::load(env_file.as_deref())?;
            build_db::run(&input, &config)
        }
        Command::GenerateSummaries { db, env_file, exclude_labels } => {
            let config = Config::load(env_file.as_deref())?;
            generate_summaries::run(&db, &config, exclude_labels.as_deref())
        }
        Command::Purge { target } => {
            let client = Client::new();
            match target {
                PurgeTarget::Nightly { older_than, env_file, dry_run } => {
                    let config = Config::load(Some(&env_file))?;
                    purge::purge_github_versions(
                        &client, &config.github_token,
                        Path::new("out/purge/nightly_github.jsonl"),
                        older_than, dry_run,
                        |t| t.contains("nightly"),
                    )?;
                    purge::purge_dockerhub_images(
                        &client, "timberio/vector",
                        &config.docker_username()?, &config.docker_password()?,
                        Path::new("out/purge/nightly_dockerhub.jsonl"),
                        30, dry_run,
                        |t| t.starts_with("nightly"),
                    )
                }
                PurgeTarget::Untagged { older_than, env_file, dry_run } => {
                    let config = Config::load(Some(&env_file))?;
                    purge::purge_github_untagged_versions(
                        &client, &config.github_token,
                        Path::new("out/purge/untagged_github.jsonl"),
                        older_than, dry_run,
                    )
                }
                PurgeTarget::VectorDev { older_than, env_file, dry_run } => {
                    let config = Config::load(Some(&env_file))?;
                    purge::purge_dockerhub_images(
                        &client, "timberio/vector-dev",
                        &config.docker_username()?, &config.docker_password()?,
                        Path::new("out/purge/vector_dev_dockerhub.jsonl"),
                        older_than, dry_run,
                        |_| true,
                    )
                }
            }
        }
    }
}
