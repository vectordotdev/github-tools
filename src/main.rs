use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use github_tools::{
    commands::{
        build_db, close_old_prs, compact, delete_stale_branches, fetch_automated_review_stats,
        fetch_discussions, fetch_issues, fetch_labels, generate_charts, generate_summaries, purge,
        push_metrics, remove_legacy_label, workflows,
    },
    config::{Config, Repo},
};
use reqwest::blocking::Client;
use std::path::Path;

#[derive(Parser)]
#[command(
    name = "github-tools",
    about = "GitHub data extraction and analysis tools"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch all labels for a repository
    FetchLabels {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
    },
    /// Fetch all issues and PRs for a repository
    FetchIssues {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
        #[arg(long, help = "Only fetch items updated since this date (ISO, YYYY-MM, or relative: 3m, 1y, 30d)")]
        since: Option<String>,
    },
    /// Fetch all discussions for a repository
    FetchDiscussions {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
        #[arg(long, help = "Only fetch items updated since this date (ISO, YYYY-MM, or relative: 3m, 1y, 30d)")]
        since: Option<String>,
    },
    /// Load issues JSON into SQLite database
    BuildDb {
        #[arg(long, help = "Path to issues JSON file")]
        input: String,
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
    },
    /// Generate CSV summaries from SQLite database
    GenerateSummaries {
        #[arg(long, help = "Path to SQLite database")]
        db: String,
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
    },
    /// Fetch issues+discussions for a repository
    FetchAll {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Only fetch items updated since this date (ISO, YYYY-MM, or relative: 3m, 1y, 30d)")]
        since: Option<String>,
    },
    /// Build DB + summaries for a repository
    GenerateAll {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Only include data from this YYYY-MM date forward. Defaults to 12 months ago.")]
        start: Option<String>,
    },
    /// Generate HTML charts from CSV summaries
    GenerateCharts {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, default_value = "out/summaries")]
        input_dir: String,
        #[arg(long, default_value = "docs")]
        output_dir: String,
        #[arg(long, help = "Only include data from this YYYY-MM date forward")]
        start: Option<String>,
    },
    /// Read the local database and submit daily historical plus current snapshots to Datadog
    PushMetrics {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file (may contain DD_API_KEY and DD_SITE)")]
        env_file: Option<String>,
        #[arg(long, help = "Datadog API key (prefer DD_API_KEY in automation)")]
        dd_api_key: Option<String>,
        #[arg(long, help = "Datadog site hostname, e.g. datadoghq.eu")]
        dd_site: Option<String>,
        #[arg(
            long,
            default_value = "30d",
            help = "Daily history to reconstruct and submit (30d, 3m, 1y)"
        )]
        history: String,
        #[arg(
            long,
            default_value = "30d",
            help = "Rolling window for closed items: ISO date, YYYY-MM, or relative (30d, 3m, 1y)"
        )]
        since: String,
        #[arg(long, default_value = "github.health")]
        prefix: String,
        #[arg(long, help = "Build and print the metric summary without sending it")]
        dry_run: bool,
        #[arg(
            long,
            conflicts_with = "dry_run",
            help = "Print prepared Datadog request batches as JSON without sending them"
        )]
        output_json: bool,
    },
    /// Fetch GitHub data locally, rebuild the database, and publish Datadog metrics
    SyncMetrics {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file (may contain GITHUB_TOKEN, DD_API_KEY, DD_SITE)")]
        env_file: Option<String>,
        #[arg(long, help = "Datadog API key (prefer DD_API_KEY in automation)")]
        dd_api_key: Option<String>,
        #[arg(long, help = "Datadog site hostname, e.g. datadoghq.eu")]
        dd_site: Option<String>,
        #[arg(
            long,
            default_value = "30d",
            help = "Historical emission lookback: ISO date, YYYY-MM, or relative (30d, 3m, 1y)"
        )]
        lookback: String,
        #[arg(
            long,
            default_value = "30d",
            help = "Rolling window for closed-item metrics (30d, 3m, 1y)"
        )]
        activity_window: String,
        #[arg(long, default_value = "github.health")]
        prefix: String,
        #[arg(
            long,
            help = "Fetch data and build metrics, but do not send to Datadog"
        )]
        dry_run: bool,
        #[arg(
            long,
            conflicts_with = "dry_run",
            help = "Fetch data and print prepared Datadog request batches as JSON without sending them"
        )]
        output_json: bool,
    },
    /// Run all purge operations (replaces purge_all.sh)
    PurgeAll {
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
    },
    /// Close old PRs with the 'meta: awaiting author' label
    CloseOldPrs {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
    },
    /// Delete branches with no commits in the last 4 years
    DeleteStaleBranches {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
    },
    /// Remove a legacy type label from issues/PRs, optionally setting the type field
    RemoveLegacyLabel {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
        #[arg(long, required = true)]
        legacy_label: String,
        #[arg(long, default_value = "open")]
        state: String,
        #[arg(long)]
        set_type_field: bool,
        #[arg(long, help = "Overwrite existing type field if already set to a different value")]
        overwrite_type: bool,
        #[arg(long)]
        require_type_field: bool,
        #[arg(long)]
        case_insensitive: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Count automated review comments by reaction (liked / disliked / no reaction).
    /// Omit --bot-login to list all review comment authors and discover the right login.
    AutomatedReviewStats {
        #[arg(long, help = "Repository, e.g. vectordotdev/vector")]
        repo: String,
        #[arg(long, help = "Path to .env file")]
        env_file: Option<String>,
        #[arg(long, help = "GitHub login of the review bot; omit to list all authors")]
        bot_login: Option<String>,
        #[arg(long, help = "Only scan PRs merged since this date (ISO, YYYY-MM, or relative: 3m, 1y, 30d)")]
        since: Option<String>,
    },
    /// Deduplicate JSON year files in a data directory
    Compact {
        #[arg(help = "Path to directory containing year JSON files (e.g. data/vectordotdev_vector/issues)")]
        dir: String,
    },
    /// Purge stale container images
    Purge {
        #[command(subcommand)]
        target: PurgeTarget,
    },
}

#[derive(Subcommand)]
enum PurgeTarget {
    /// Purge Docker Hub images
    Dockerhub {
        #[command(subcommand)]
        target: PurgeDockerHubTarget,
    },
    /// Purge GitHub container registry images
    Github {
        #[command(subcommand)]
        target: PurgeGithubTarget,
    },
}

#[derive(Subcommand)]
enum PurgeDockerHubTarget {
    /// Purge old nightly images from Docker Hub
    Nightly {
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
    },
    /// Purge old vector-dev images from Docker Hub
    VectorDev {
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum PurgeGithubTarget {
    /// Purge old nightly images from GitHub container registry
    Nightly {
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
    },
    /// Purge untagged GitHub container images
    Untagged {
        #[arg(long, default_value = "30")]
        older_than: u32,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = "Skip confirmation prompt")]
        yes: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::FetchLabels { repo, env_file } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            fetch_labels::run(&config)
        }
        Command::FetchIssues { repo, env_file, since } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            fetch_issues::run(&config, since.as_deref())
        }
        Command::FetchDiscussions { repo, env_file, since } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            fetch_discussions::run(&config, since.as_deref())
        }
        Command::BuildDb { input, repo, env_file } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            build_db::run(&input, &config)
        }
        Command::AutomatedReviewStats { repo, env_file, bot_login, since } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            fetch_automated_review_stats::run(&config, bot_login.as_deref(), since.as_deref())
        }
        Command::Compact { dir } => compact::run(&dir),
        Command::FetchAll { repo, since } => workflows::fetch_all(&repo, since.as_deref()),
        Command::GenerateAll { repo, start } => workflows::generate_all(&repo, start.as_deref()),
        Command::GenerateCharts { repo, input_dir, output_dir, start } => {
            generate_charts::run(&input_dir, &repo, &output_dir, start.as_deref())
        }
        Command::PushMetrics {
            repo,
            env_file,
            dd_api_key,
            dd_site,
            history,
            since,
            prefix,
            dry_run,
            output_json,
        } => {
            load_env_file(env_file.as_deref())?;
            let config = Config::for_repo(&Repo::parse(&repo)?);
            let api_key = dd_api_key.or_else(|| std::env::var("DD_API_KEY").ok());
            let site = dd_site.or_else(|| std::env::var("DD_SITE").ok());
            push_metrics::run(
                &config,
                api_key.as_deref(),
                site.as_deref(),
                Some(&history),
                Some(&since),
                Some(&prefix),
                dry_run,
                output_json,
            )
        }
        Command::SyncMetrics {
            repo,
            env_file,
            dd_api_key,
            dd_site,
            lookback,
            activity_window,
            prefix,
            dry_run,
            output_json,
        } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            let api_key = dd_api_key.or_else(|| std::env::var("DD_API_KEY").ok());
            let site = dd_site.or_else(|| std::env::var("DD_SITE").ok());
            workflows::sync_metrics(
                &config,
                Some(&lookback),
                api_key.as_deref(),
                site.as_deref(),
                Some(&activity_window),
                Some(&prefix),
                dry_run,
                output_json,
            )
        }
        Command::PurgeAll { older_than, dry_run, yes } => {
            workflows::purge_all(older_than, dry_run, yes)
        }
        Command::CloseOldPrs { repo, env_file, dry_run, yes } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            close_old_prs::run(&config, dry_run, yes)
        }
        Command::DeleteStaleBranches { repo, env_file, dry_run, yes } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            delete_stale_branches::run(&config, dry_run, yes)
        }
        Command::GenerateSummaries { db, repo, env_file } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            generate_summaries::run(&db, &config)
        }
        Command::RemoveLegacyLabel {
            repo,
            env_file,
            legacy_label,
            state,
            set_type_field,
            overwrite_type,
            require_type_field,
            case_insensitive,
            dry_run,
            yes,
            since,
            limit,
        } => {
            let config = Config::load(&Repo::parse(&repo)?, env_file.as_deref())?;
            remove_legacy_label::run(&remove_legacy_label::Args {
                repos: vec![format!("{}/{}", config.org, config.repo)],
                legacy_label,
                state,
                set_type_field,
                overwrite_type,
                require_type_field,
                case_insensitive,
                token: config.github_token,
                dry_run,
                since,
                limit,
                yes,
            })
        }
        Command::Purge { target } => {
            let client = Client::new();
            match target {
                PurgeTarget::Dockerhub { target } => {
                    let docker_username =
                        std::env::var("DOCKER_USERNAME").context("DOCKER_USERNAME not set")?;
                    let docker_password =
                        std::env::var("DOCKER_PASSWORD").context("DOCKER_PASSWORD not set")?;
                    match target {
                        PurgeDockerHubTarget::Nightly { older_than, dry_run, yes } => {
                            let dh_repo = std::env::var("DOCKERHUB_NIGHTLY_REPO")
                                .unwrap_or_else(|_| "timberio/vector".to_string());
                            purge::purge_dockerhub_images(
                                &client,
                                &dh_repo,
                                &docker_username,
                                &docker_password,
                                Path::new("out/purge/nightly_dockerhub.jsonl"),
                                older_than,
                                dry_run,
                                yes,
                                |t| t.starts_with("nightly"),
                            )
                        }
                        PurgeDockerHubTarget::VectorDev { older_than, dry_run, yes } => {
                            let dh_repo = std::env::var("DOCKERHUB_VECTOR_DEV_REPO")
                                .unwrap_or_else(|_| "timberio/vector-dev".to_string());
                            purge::purge_dockerhub_images(
                                &client,
                                &dh_repo,
                                &docker_username,
                                &docker_password,
                                Path::new("out/purge/vector_dev_dockerhub.jsonl"),
                                older_than,
                                dry_run,
                                yes,
                                |_| true,
                            )
                        }
                    }
                }
                PurgeTarget::Github { target } => {
                    let github_token =
                        std::env::var("GITHUB_TOKEN").context("GITHUB_TOKEN not set")?;
                    match target {
                        PurgeGithubTarget::Nightly { older_than, dry_run, yes } => {
                            purge::purge_github_versions(
                                &client,
                                &github_token,
                                Path::new("out/purge/nightly_github.jsonl"),
                                older_than,
                                dry_run,
                                yes,
                                |t| t.contains("nightly"),
                            )
                        }
                        PurgeGithubTarget::Untagged { older_than, dry_run, yes } => {
                            purge::purge_github_untagged_versions(
                                &client,
                                &github_token,
                                Path::new("out/purge/untagged_github.jsonl"),
                                older_than,
                                dry_run,
                                yes,
                            )
                        }
                    }
                }
            }
        }
    }
}

fn load_env_file(path: Option<&str>) -> Result<()> {
    if let Some(path) = path {
        dotenvy::from_filename_override(path)
            .with_context(|| format!("Failed to load env file: {path}"))?;
    } else {
        dotenvy::dotenv().ok();
    }
    Ok(())
}
