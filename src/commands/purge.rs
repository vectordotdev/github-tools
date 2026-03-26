use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

const GITHUB_ORG: &str = "vectordotdev";
const GITHUB_PACKAGE: &str = "vector";
const DOCKERHUB_LOGIN_URL: &str = "https://hub.docker.com/v2/users/login/";

fn blocked_tag_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\d+\.\d+\.\d+([.-]|$)|^\d+\.\d+\.X([.-]|$)").unwrap()
    })
}

fn threshold(days_old: u32) -> DateTime<Utc> {
    Utc::now() - Duration::days(days_old as i64)
}

// ── GitHub GHCR ──────────────────────────────────────────────────────────────

fn github_api_url() -> String {
    format!(
        "https://api.github.com/orgs/{GITHUB_ORG}/packages/container/{GITHUB_PACKAGE}/versions"
    )
}

fn list_github_versions(client: &Client, token: &str) -> Result<Vec<Value>> {
    let url = github_api_url();
    let mut versions = Vec::new();
    let mut page = 1u32;

    loop {
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "github-tools")
            .query(&[("per_page", "100"), ("page", &page.to_string())])
            .send()
            .context("Failed to list GitHub versions")?;

        resp.error_for_status_ref()
            .with_context(|| format!("GitHub versions API error on page {page}"))?;

        let batch: Vec<Value> = resp.json()?;
        if batch.is_empty() {
            break;
        }
        versions.extend(batch);
        page += 1;
    }
    Ok(versions)
}

fn delete_github_version(client: &Client, version_id: u64, token: &str) -> Result<bool> {
    let resp = client
        .delete(format!("{}/{version_id}", github_api_url()))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "github-tools")
        .send()?;
    Ok(resp.status().as_u16() == 204)
}

pub fn purge_github_versions(
    client: &Client,
    token: &str,
    audit_file: &Path,
    older_than: u32,
    dry_run: bool,
    tag_filter: impl Fn(&str) -> bool,
) -> Result<()> {
    let cutoff = threshold(older_than);
    println!("Checking GitHub container versions older than {}", cutoff.date_naive());

    let mut audit = open_audit(audit_file, dry_run)?;
    let versions = list_github_versions(client, token)?;
    println!("Fetched {} GitHub versions", versions.len());

    for version in &versions {
        let tags: Vec<&str> = version["metadata"]["container"]["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let created_at = version["created_at"].as_str().unwrap_or("");
        let created_dt = created_at
            .trim_end_matches('Z')
            .parse::<chrono::NaiveDateTime>()
            .map(|dt| dt.and_utc())
            .unwrap_or(Utc::now());

        if created_dt >= cutoff {
            continue;
        }

        let matching: Vec<&str> = tags
            .iter()
            .copied()
            .filter(|t| !blocked_tag_pattern().is_match(t) && tag_filter(t))
            .collect();

        if matching.is_empty() {
            continue;
        }

        let version_id = version["id"].as_u64().unwrap_or(0);
        println!("Found version {version_id} (tags: {tags:?}, created: {})", created_dt.date_naive());

        if dry_run {
            for tag in &matching {
                writeln!(audit, "{}", json!({"tag": tag, "last_updated": created_dt.date_naive().to_string()}))?;
            }
        } else {
            if delete_github_version(client, version_id, token)? {
                println!("Deleted GitHub version {version_id}");
                for tag in &matching {
                    writeln!(audit, "{}", json!({"tag": tag, "last_updated": created_dt.date_naive().to_string()}))?;
                }
            } else {
                eprintln!("Failed to delete GitHub version {version_id}");
            }
        }
    }
    println!("Audit log saved to: {}", audit_file.display());
    Ok(())
}

pub fn purge_github_untagged_versions(
    client: &Client,
    token: &str,
    audit_file: &Path,
    older_than: u32,
    dry_run: bool,
) -> Result<()> {
    let cutoff = threshold(older_than);
    println!("Checking untagged GitHub container versions older than {}", cutoff.date_naive());

    let mut audit = open_audit(audit_file, dry_run)?;
    let versions = list_github_versions(client, token)?;
    println!("Fetched {} GitHub versions", versions.len());

    for version in &versions {
        let tags: Vec<&str> = version["metadata"]["container"]["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        if !tags.is_empty() {
            continue;
        }

        let created_at = version["created_at"].as_str().unwrap_or("");
        let created_dt = created_at
            .trim_end_matches('Z')
            .parse::<chrono::NaiveDateTime>()
            .map(|dt| dt.and_utc())
            .unwrap_or(Utc::now());

        if created_dt >= cutoff {
            continue;
        }

        let version_id = version["id"].as_u64().unwrap_or(0);
        println!("Untagged version {version_id} (created: {})", created_dt.date_naive());

        if dry_run {
            writeln!(audit, "{}", json!({"tag": "<untagged>", "version_id": version_id, "last_updated": created_dt.date_naive().to_string()}))?;
        } else {
            if delete_github_version(client, version_id, token)? {
                println!("Deleted untagged GitHub version {version_id}");
                writeln!(audit, "{}", json!({"tag": "<untagged>", "version_id": version_id, "last_updated": created_dt.date_naive().to_string()}))?;
            } else {
                eprintln!("Failed to delete untagged GitHub version {version_id}");
            }
        }
    }
    println!("Audit log saved to: {}", audit_file.display());
    Ok(())
}

// ── Docker Hub ───────────────────────────────────────────────────────────────

fn dockerhub_login(client: &Client, username: &str, password: &str) -> Result<String> {
    let resp = client
        .post(DOCKERHUB_LOGIN_URL)
        .json(&json!({"username": username, "password": password}))
        .send()?;

    if resp.status().as_u16() != 200 {
        anyhow::bail!("Failed to authenticate with Docker Hub: {}", resp.text()?);
    }
    Ok(resp.json::<Value>()?["token"]
        .as_str()
        .context("missing token in Docker Hub login response")?
        .to_string())
}

fn list_dockerhub_tags(client: &Client, repo: &str) -> Result<Vec<Value>> {
    let base = format!("https://hub.docker.com/v2/repositories/{repo}/tags?page_size=100");
    let mut tags = Vec::new();
    let mut page = 1u32;

    loop {
        let resp = client
            .get(format!("{base}&page={page}"))
            .send()?
            .error_for_status()?;
        let data: Value = resp.json()?;
        let results = data["results"].as_array().cloned().unwrap_or_default();
        let done = data["next"].is_null();
        tags.extend(results);
        if done { break; }
        page += 1;
    }
    Ok(tags)
}

pub fn purge_dockerhub_images(
    client: &Client,
    repo: &str,
    username: &str,
    password: &str,
    audit_file: &Path,
    older_than: u32,
    dry_run: bool,
    tag_filter: impl Fn(&str) -> bool,
) -> Result<()> {
    let cutoff = threshold(older_than);
    println!("Checking Docker Hub tags for '{repo}' older than {}", cutoff.date_naive());

    let token = dockerhub_login(client, username, password)?;
    let auth_header = format!("JWT {token}");

    let mut audit = open_audit(audit_file, dry_run)?;
    let tags = list_dockerhub_tags(client, repo)?;

    for tag in &tags {
        let name = tag["name"].as_str().unwrap_or("");

        if blocked_tag_pattern().is_match(name) {
            continue;
        }

        let last_updated = tag["last_updated"].as_str().unwrap_or("");
        let tag_date = last_updated
            .replace('Z', "+00:00")
            .parse::<DateTime<Utc>>()
            .unwrap_or(Utc::now());

        if tag_date >= cutoff || !tag_filter(name) {
            continue;
        }

        println!("Found tag: {name} (last updated: {})", tag_date.date_naive());

        if dry_run {
            writeln!(audit, "{}", json!({"tag": name, "last_updated": tag_date.date_naive().to_string()}))?;
        } else {
            let delete_url = format!("https://hub.docker.com/v2/repositories/{repo}/tags/{name}/");
            let resp = client.delete(&delete_url).header("Authorization", &auth_header).send()?;
            if resp.status().as_u16() == 204 {
                println!("Deleted tag: {name}");
                writeln!(audit, "{}", json!({"tag": name, "last_updated": tag_date.date_naive().to_string()}))?;
            } else {
                eprintln!("Failed to delete {name}: {} - {}", resp.status(), resp.text()?);
            }
        }
    }
    println!("Audit log saved to: {}", audit_file.display());
    Ok(())
}

// ── Shared ───────────────────────────────────────────────────────────────────

fn open_audit(path: &Path, dry_run: bool) -> Result<impl Write> {
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let mut f = OpenOptions::new().create(true).write(true).truncate(true).open(path)
        .with_context(|| format!("Failed to open audit file: {}", path.display()))?;
    writeln!(f, "{}", json!({"dry_run": dry_run}))?;
    Ok(f)
}
