use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

const DOCKERHUB_LOGIN_URL: &str = "https://hub.docker.com/v2/users/login/";

// Configurable via env vars; fall back to vectordotdev/vector defaults.
fn ghcr_org() -> String {
    std::env::var("GHCR_ORG").unwrap_or_else(|_| "vectordotdev".to_string())
}
fn ghcr_package() -> String {
    std::env::var("GHCR_PACKAGE").unwrap_or_else(|_| "vector".to_string())
}

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
        "https://api.github.com/orgs/{}/packages/container/{}/versions",
        ghcr_org(), ghcr_package()
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
    yes: bool,
    tag_filter: impl Fn(&str) -> bool,
) -> Result<()> {
    let cutoff = threshold(older_than);
    println!("Checking {}/{} container versions older than {}", ghcr_org(), ghcr_package(), cutoff.date_naive());

    let versions = list_github_versions(client, token)?;
    println!("Fetched {} GitHub versions", versions.len());

    // Pre-collect matching versions
    let matching: Vec<(&Value, Vec<String>)> = versions.iter().filter_map(|version| {
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

        if created_dt >= cutoff { return None; }

        let matched_tags: Vec<String> = tags.iter()
            .copied()
            .filter(|t| !blocked_tag_pattern().is_match(t) && tag_filter(t))
            .map(|t| t.to_string())
            .collect();

        if matched_tags.is_empty() { None } else { Some((version, matched_tags)) }
    }).collect();

    if matching.is_empty() {
        println!("No matching versions found.");
        open_audit(audit_file, dry_run)?; // still write header
        return Ok(());
    }

    println!("Found {} version(s) to purge.", matching.len());

    if dry_run {
        let mut audit = open_audit(audit_file, dry_run)?;
        for (version, tags) in &matching {
            let created_at = version["created_at"].as_str().unwrap_or("").trim_end_matches('Z');
            let created_dt = created_at.parse::<chrono::NaiveDateTime>().map(|dt| dt.and_utc()).unwrap_or(Utc::now());
            for tag in tags {
                writeln!(audit, "{}", json!({"tag": tag, "last_updated": created_dt.date_naive().to_string()}))?;
            }
        }
        println!("[dry-run] Audit log saved to: {}", audit_file.display());
        return Ok(());
    }

    if !crate::confirm(&format!("Delete {} GitHub container version(s)?", matching.len()), yes) {
        println!("Aborted.");
        return Ok(());
    }

    let mut audit = open_audit(audit_file, dry_run)?;
    for (version, tags) in &matching {
        let version_id = version["id"].as_u64().unwrap_or(0);
        let created_at = version["created_at"].as_str().unwrap_or("").trim_end_matches('Z');
        let created_dt = created_at.parse::<chrono::NaiveDateTime>().map(|dt| dt.and_utc()).unwrap_or(Utc::now());

        println!("Deleting version {version_id} (tags: {tags:?})");
        if delete_github_version(client, version_id, token)? {
            println!("Deleted GitHub version {version_id}");
            for tag in tags {
                writeln!(audit, "{}", json!({"tag": tag, "last_updated": created_dt.date_naive().to_string()}))?;
            }
        } else {
            eprintln!("Failed to delete GitHub version {version_id}");
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
    yes: bool,
) -> Result<()> {
    let cutoff = threshold(older_than);
    println!("Checking untagged {}/{} container versions older than {}", ghcr_org(), ghcr_package(), cutoff.date_naive());

    let versions = list_github_versions(client, token)?;
    println!("Fetched {} GitHub versions", versions.len());

    let untagged: Vec<(&Value, DateTime<Utc>)> = versions.iter().filter_map(|version| {
        let tags: Vec<&str> = version["metadata"]["container"]["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        if !tags.is_empty() { return None; }

        let created_at = version["created_at"].as_str().unwrap_or("").trim_end_matches('Z');
        let created_dt = created_at.parse::<chrono::NaiveDateTime>().map(|dt| dt.and_utc()).unwrap_or(Utc::now());

        if created_dt >= cutoff { None } else { Some((version, created_dt)) }
    }).collect();

    if untagged.is_empty() {
        println!("No untagged versions found.");
        open_audit(audit_file, dry_run)?;
        return Ok(());
    }

    println!("Found {} untagged version(s) to purge.", untagged.len());

    if dry_run {
        let mut audit = open_audit(audit_file, dry_run)?;
        for (version, created_dt) in &untagged {
            let version_id = version["id"].as_u64().unwrap_or(0);
            writeln!(audit, "{}", json!({"tag": "<untagged>", "version_id": version_id, "last_updated": created_dt.date_naive().to_string()}))?;
        }
        println!("[dry-run] Audit log saved to: {}", audit_file.display());
        return Ok(());
    }

    if !crate::confirm(&format!("Delete {} untagged GitHub container version(s)?", untagged.len()), yes) {
        println!("Aborted.");
        return Ok(());
    }

    let mut audit = open_audit(audit_file, dry_run)?;
    for (version, created_dt) in &untagged {
        let version_id = version["id"].as_u64().unwrap_or(0);
        if delete_github_version(client, version_id, token)? {
            println!("Deleted untagged GitHub version {version_id}");
            writeln!(audit, "{}", json!({"tag": "<untagged>", "version_id": version_id, "last_updated": created_dt.date_naive().to_string()}))?;
        } else {
            eprintln!("Failed to delete untagged GitHub version {version_id}");
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
        // Do not include response body — it may echo credentials on some errors
        anyhow::bail!("Failed to authenticate with Docker Hub (HTTP {})", resp.status());
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
    yes: bool,
    tag_filter: impl Fn(&str) -> bool,
) -> Result<()> {
    let cutoff = threshold(older_than);
    println!("Checking Docker Hub '{repo}' tags older than {}", cutoff.date_naive());

    let tags = list_dockerhub_tags(client, repo)?;

    let matching: Vec<(&Value, String, DateTime<Utc>)> = tags.iter().filter_map(|tag| {
        let name = tag["name"].as_str().unwrap_or("");
        if blocked_tag_pattern().is_match(name) { return None; }

        let last_updated = tag["last_updated"].as_str().unwrap_or("");
        let tag_date = last_updated.replace('Z', "+00:00").parse::<DateTime<Utc>>().unwrap_or(Utc::now());

        if tag_date >= cutoff || !tag_filter(name) { None } else { Some((tag, name.to_string(), tag_date)) }
    }).collect();

    if matching.is_empty() {
        println!("No matching tags found.");
        open_audit(audit_file, dry_run)?;
        return Ok(());
    }

    println!("Found {} tag(s) to purge.", matching.len());

    if dry_run {
        let mut audit = open_audit(audit_file, dry_run)?;
        for (_, name, tag_date) in &matching {
            writeln!(audit, "{}", json!({"tag": name, "last_updated": tag_date.date_naive().to_string()}))?;
        }
        println!("[dry-run] Audit log saved to: {}", audit_file.display());
        return Ok(());
    }

    if !crate::confirm(&format!("Delete {} Docker Hub tag(s) from '{repo}'?", matching.len()), yes) {
        println!("Aborted.");
        return Ok(());
    }

    // Authenticate only when we're actually going to delete
    let token = dockerhub_login(client, username, password)?;
    let auth_header = format!("JWT {token}");
    let mut audit = open_audit(audit_file, dry_run)?;

    for (_, name, tag_date) in &matching {
        let delete_url = format!("https://hub.docker.com/v2/repositories/{repo}/tags/{name}/");
        let resp = client.delete(&delete_url).header("Authorization", &auth_header).send()?;
        if resp.status().as_u16() == 204 {
            println!("Deleted tag: {name}");
            writeln!(audit, "{}", json!({"tag": name, "last_updated": tag_date.date_naive().to_string()}))?;
        } else {
            eprintln!("Failed to delete {name}: {}", resp.status());
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
