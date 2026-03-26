use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::thread;
use std::time::Duration;

const API_ROOT: &str = "https://api.github.com";

pub struct Args {
    pub repos: Vec<String>,
    pub legacy_label: String,
    pub state: String,
    pub set_type_field: bool,
    pub require_type_field: bool,
    pub case_insensitive: bool,
    pub token: String,
    pub dry_run: bool,
    pub since: Option<String>,
    pub limit: Option<usize>,
}

struct TypeInfo {
    name: &'static str,
}

fn type_mapping() -> HashMap<&'static str, TypeInfo> {
    HashMap::from([
        ("type: bug",     TypeInfo { name: "Bug" }),
        ("type: feature", TypeInfo { name: "Feature" }),
        ("type: task",    TypeInfo { name: "Task" }),
    ])
}

fn gh_headers(token: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("Authorization", format!("Bearer {token}").parse().unwrap());
    headers.insert("Accept", "application/vnd.github+json".parse().unwrap());
    headers.insert("X-GitHub-Api-Version", "2022-11-28".parse().unwrap());
    headers.insert("User-Agent", "label-cleanup-script/1.0".parse().unwrap());
    headers
}

fn backoff_sleep(attempt: u32) {
    let secs = (2u64.pow(attempt)).min(60);
    thread::sleep(Duration::from_secs(secs));
}

fn iter_issues(client: &Client, repo: &str, token: &str, state: &str, label: &str) -> Result<Vec<Value>> {
    let url = format!("{API_ROOT}/repos/{repo}/issues");
    let mut all = Vec::new();
    let mut page = 1u32;

    loop {
        let resp = client
            .get(&url)
            .headers(gh_headers(token))
            .query(&[
                ("state", state),
                ("labels", label),
                ("direction", "asc"),
                ("sort", "created"),
                ("per_page", "100"),
                ("page", &page.to_string()),
            ])
            .send()?;

        // Rate-limit handling
        if resp.status().as_u16() == 403 {
            let body = resp.text()?;
            if body.to_lowercase().contains("rate limit") {
                println!("[rate-limit] sleeping 60s...");
                thread::sleep(Duration::from_secs(60));
                continue;
            }
            anyhow::bail!("403 response: {body}");
        }

        let batch: Vec<Value> = resp.error_for_status()?.json()?;
        let done = batch.len() < 100;
        all.extend(batch);
        if done { break; }
        page += 1;
    }
    Ok(all)
}

fn has_label(item: &Value, name: &str, case_insensitive: bool) -> bool {
    item["labels"]
        .as_array()
        .map(|labels| labels.iter().any(|l| {
            let lname = l["name"].as_str().unwrap_or("");
            if case_insensitive { lname.to_lowercase() == name.to_lowercase() }
            else { lname == name }
        }))
        .unwrap_or(false)
}

fn set_type_field(client: &Client, repo: &str, token: &str, number: u64, type_name: &str, dry_run: bool) -> (bool, String) {
    if dry_run {
        return (true, format!("[dry-run] would SET type to '{type_name}'"));
    }
    let url = format!("{API_ROOT}/repos/{repo}/issues/{number}");
    for attempt in 0..5u32 {
        let resp = match client.patch(&url).headers(gh_headers(token)).json(&json!({"type": type_name})).send() {
            Ok(r) => r,
            Err(e) => { backoff_sleep(attempt + 1); eprintln!("Request error: {e}"); continue; }
        };
        let status = resp.status().as_u16();
        if status == 200 || status == 201 {
            let result: Value = resp.json().unwrap_or_default();
            let actual = result["type"]["name"].as_str().unwrap_or("");
            if actual == type_name {
                return (true, format!("Set type to '{type_name}'"));
            } else if actual.is_empty() {
                return (false, "Type field not supported (issue may not be in project)".into());
            } else {
                return (false, format!("Type set to '{actual}' instead of '{type_name}'"));
            }
        }
        if status == 403 || (500..600).contains(&status) {
            backoff_sleep(attempt + 1);
            continue;
        }
        let body = resp.text().unwrap_or_default();
        return (false, format!("Failed to set type: {status} {body}"));
    }
    (false, "Failed to set type after retries".into())
}

fn remove_label(client: &Client, repo: &str, token: &str, number: u64, label: &str, dry_run: bool) -> (bool, String) {
    if dry_run {
        let encoded = urlencoding::encode(label);
        return (true, format!("[dry-run] would DELETE /repos/{repo}/issues/{number}/labels/{encoded}"));
    }
    let encoded = urlencoding::encode(label);
    let url = format!("{API_ROOT}/repos/{repo}/issues/{number}/labels/{encoded}");
    for attempt in 0..5u32 {
        let resp = match client.delete(&url).headers(gh_headers(token)).send() {
            Ok(r) => r,
            Err(e) => { backoff_sleep(attempt + 1); eprintln!("Request error: {e}"); continue; }
        };
        let status = resp.status().as_u16();
        if status == 204 || status == 200 { return (true, format!("Removed label '{label}' from #{number}")); }
        if status == 404 { return (false, format!("Label '{label}' not present on #{number} (404)")); }
        if status == 403 || (500..600).contains(&status) { backoff_sleep(attempt + 1); continue; }
        let body = resp.text().unwrap_or_default();
        return (false, format!("Failed to remove label on #{number}: {status} {body}"));
    }
    (false, format!("Failed to remove label on #{number} after retries"))
}

pub fn run(args: &Args) -> Result<()> {
    let client = Client::new();
    let mapping = type_mapping();

    let key = if args.case_insensitive {
        args.legacy_label.to_lowercase()
    } else {
        args.legacy_label.clone()
    };

    let type_info = mapping
        .iter()
        .find(|(k, _)| if args.case_insensitive { k.to_lowercase() == key } else { **k == key })
        .map(|(_, v)| v)
        .with_context(|| format!(
            "'{}' is not a supported legacy label. Supported: {}",
            args.legacy_label,
            mapping.keys().cloned().collect::<Vec<_>>().join(", ")
        ))?;

    let since_date = args.since.as_deref().map(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .with_context(|| format!("Invalid date '{s}', use YYYY-MM-DD"))
    }).transpose()?;

    if let Some(d) = &since_date {
        println!("Filtering to issues created after {d}");
    }
    println!("Migrating '{}' -> Type field '{}'", args.legacy_label, type_info.name);
    if let Some(limit) = args.limit {
        println!("Processing up to {limit} issues");
    }

    let mut total_seen = 0usize;
    let mut total_changed = 0usize;
    let mut total_skipped = 0usize;
    let mut processed_count = 0usize;

    'repos: for repo in &args.repos {
        println!("\n=== Repo: {repo} ===");
        let items = iter_issues(&client, repo, &args.token, &args.state, &args.legacy_label)?;

        for item in &items {
            let number = item["number"].as_u64().unwrap_or(0);
            let title = item["title"].as_str().unwrap_or("");
            total_seen += 1;

            if args.limit.map(|l| processed_count >= l).unwrap_or(false) {
                println!("Reached limit of {}. Stopping.", args.limit.unwrap());
                break 'repos;
            }

            // Date filter
            if let Some(since) = &since_date {
                let created_str = item["created_at"].as_str().unwrap_or("");
                let created = chrono::NaiveDateTime::parse_from_str(created_str, "%Y-%m-%dT%H:%M:%SZ")
                    .map(|dt| dt.date())
                    .unwrap_or(*since);
                if created < *since {
                    total_skipped += 1;
                    println!("#{number}  SKIP (created {created}, before {since})  {title}");
                    continue;
                }
            }

            // Defensive label check
            if !has_label(item, &args.legacy_label, args.case_insensitive) {
                total_skipped += 1;
                println!("#{number}  (no legacy label)  {title}");
                continue;
            }

            // Check current type field
            let current_type_name = item["type"]["name"].as_str().unwrap_or("");
            let has_correct_type = current_type_name == type_info.name;

            if args.require_type_field && !has_correct_type {
                total_skipped += 1;
                println!("#{number}  SKIP (type field not set to '{}')  {title}", type_info.name);
                continue;
            }

            processed_count += 1;

            let mut set_ok = true;
            if args.set_type_field && !has_correct_type {
                let (ok, msg) = set_type_field(&client, repo, &args.token, number, type_info.name, args.dry_run);
                println!("#{number}  {msg}  {title}");
                set_ok = ok;
                if !ok && !args.dry_run {
                    total_skipped += 1;
                    println!("#{number}  ERROR: Cannot set type field. Keeping legacy label and stopping.");
                    println!("\nStopping due to type field error.");
                    break 'repos;
                }
            }

            if args.set_type_field && !has_correct_type && !set_ok {
                total_skipped += 1;
                println!("#{number}  SKIP: Not removing label since type couldn't be set");
            } else {
                let (changed, msg) = remove_label(&client, repo, &args.token, number, &args.legacy_label, args.dry_run);
                if changed { total_changed += 1; } else { total_skipped += 1; }
                println!("#{number}  {msg}  {title}");
            }
        }
    }

    println!(
        "\nDone. Items seen={total_seen}, processed={processed_count}, changed={total_changed}, skipped/unchanged={total_skipped}."
    );
    Ok(())
}
