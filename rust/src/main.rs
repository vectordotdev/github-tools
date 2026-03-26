use anyhow::Result;
use clap::{Parser, Subcommand};

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
            todo!("fetch-labels: env_file={env_file:?}")
        }
        Command::FetchIssues { env_file } => {
            todo!("fetch-issues: env_file={env_file:?}")
        }
        Command::FetchDiscussions { env_file } => {
            todo!("fetch-discussions: env_file={env_file:?}")
        }
        Command::BuildDb { input, env_file } => {
            todo!("build-db: input={input}, env_file={env_file:?}")
        }
        Command::GenerateSummaries { db, env_file, exclude_labels } => {
            todo!("generate-summaries: db={db}, env_file={env_file:?}, exclude_labels={exclude_labels:?}")
        }
        Command::Purge { target } => match target {
            PurgeTarget::Nightly { older_than, env_file, dry_run } => {
                todo!("purge-nightly: older_than={older_than}, env_file={env_file}, dry_run={dry_run}")
            }
            PurgeTarget::Untagged { older_than, env_file, dry_run } => {
                todo!("purge-untagged: older_than={older_than}, env_file={env_file}, dry_run={dry_run}")
            }
            PurgeTarget::VectorDev { older_than, env_file, dry_run } => {
                todo!("purge-vector-dev: older_than={older_than}, env_file={env_file}, dry_run={dry_run}")
            }
        },
    }
}
