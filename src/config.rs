use anyhow::{Context, Result};
use std::env;

pub struct Repo {
    pub org: String,
    pub name: String,
}

impl Repo {
    pub fn parse(s: &str) -> Result<Self> {
        let (org, name) = s
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("expected org/name, got {s:?}"))?;
        Ok(Self { org: org.to_string(), name: name.to_string() })
    }
}

pub struct Config {
    pub github_token: String,
    pub org: String,
    pub repo: String,
}

impl Config {
    /// Load credentials from an env file (if given) and pair with an explicit repo.
    pub fn load(repo: &Repo, env_file: Option<&str>) -> Result<Self> {
        if let Some(path) = env_file {
            dotenvy::from_filename_override(path)
                .with_context(|| format!("Failed to load env file: {path}"))?;
        } else {
            dotenvy::dotenv().ok();
        }
        Ok(Self {
            github_token: env::var("GITHUB_TOKEN").context("GITHUB_TOKEN not set")?,
            org: repo.org.clone(),
            repo: repo.name.clone(),
        })
    }

    /// Read credentials from the current environment (no file loading).
    pub fn for_repo(repo: &Repo) -> Result<Self> {
        Ok(Self {
            github_token: env::var("GITHUB_TOKEN").context("GITHUB_TOKEN not set")?,
            org: repo.org.clone(),
            repo: repo.name.clone(),
        })
    }
}
