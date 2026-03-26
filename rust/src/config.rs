use anyhow::{Context, Result};
use std::env;

pub struct Config {
    pub github_token: String,
    pub repo_owner: String,
    pub repo_name: String,
}

impl Config {
    pub fn docker_username(&self) -> Result<String> {
        env::var("DOCKER_USERNAME").context("DOCKER_USERNAME not set")
    }

    pub fn docker_password(&self) -> Result<String> {
        env::var("DOCKER_PASSWORD").context("DOCKER_PASSWORD not set")
    }
}

impl Config {
    pub fn load(env_file: Option<&str>) -> Result<Self> {
        if let Some(path) = env_file {
            dotenvy::from_filename(path)
                .with_context(|| format!("Failed to load env file: {path}"))?;
        } else {
            dotenvy::dotenv().ok();
        }

        Ok(Self {
            github_token: env::var("GITHUB_TOKEN").context("GITHUB_TOKEN not set")?,
            repo_owner: env::var("REPO_OWNER").context("REPO_OWNER not set")?,
            repo_name: env::var("REPO_NAME").context("REPO_NAME not set")?,
        })
    }
}
