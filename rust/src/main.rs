use anyhow::Result;
use clap::{Parser, Subcommand};
use github_tools::{commands::{build_db, close_old_prs, delete_stale_branches, fetch_discussions, fetch_issues, fetch_labels, generate_summaries, purge, remove_legacy_label, workflows}, config::Config};
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
    /// Fetch issues+discussions for all repos (replaces fetch_all_slow.sh)
    FetchAll {
        #[arg(long, required = true, help = "Env files to iterate (repeatable)")]
        env_file: Vec<String>,
    },
    /// Build DB + summaries for all repos (replaces generate_all.sh)
    GenerateAll {
        #[arg(long, required = true)]
        env_file: Vec<String>,
        #[arg(long)]
        exclude_labels: Option<String>,
    },
    /// Run all purge operations (replaces purge_all.sh)
    PurgeAll {
        #[arg(long)]
        env_file: String,
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long)]
        dry_run: bool,
    },
    /// Close old PRs with the 'meta: awaiting author' label
    CloseOldPrs {
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete branches with no commits in the last 4 years
    DeleteStaleBranches {
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
    },
    /// Remove a legacy type label from issues/PRs, optionally setting the type field
    RemoveLegacyLabel {
        #[arg(long, required = true, help = "Repository owner/name (repeatable)")]
        repo: Vec<String>,
        #[arg(long, required = true)]
        legacy_label: String,
        #[arg(long, default_value = "open")]
        state: String,
        #[arg(long)]
        set_type_field: bool,
        #[arg(long)]
        require_type_field: bool,
        #[arg(long)]
        case_insensitive: bool,
        #[arg(long, default_value = "")]
        token: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
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
        Command::FetchAll { env_file } => {
            workflows::fetch_all(&env_file)
        }
        Command::GenerateAll { env_file, exclude_labels } => {
            workflows::generate_all(&env_file, exclude_labels.as_deref())
        }
        Command::PurgeAll { env_file, older_than, dry_run } => {
            workflows::purge_all(&env_file, older_than, dry_run)
        }
        Command::CloseOldPrs { env_file, dry_run } => {
            let config = Config::load(env_file.as_deref())?;
            close_old_prs::run(&config, dry_run)
        }
        Command::DeleteStaleBranches { env_file } => {
            let config = Config::load(env_file.as_deref())?;
            delete_stale_branches::run(&config)
        }
        Command::GenerateSummaries { db, env_file, exclude_labels } => {
            let config = Config::load(env_file.as_deref())?;
            generate_summaries::run(&db, &config, exclude_labels.as_deref())
        }
        Command::RemoveLegacyLabel { repo, legacy_label, state, set_type_field, require_type_field, case_insensitive, token, dry_run, since, limit } => {
            let resolved_token = if token.is_empty() {
                std::env::var("GITHUB_TOKEN").unwrap_or_default()
            } else {
                token
            };
            remove_legacy_label::run(&remove_legacy_label::Args {
                repos: repo, legacy_label, state, set_type_field, require_type_field,
                case_insensitive, token: resolved_token, dry_run, since, limit,
            })
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
