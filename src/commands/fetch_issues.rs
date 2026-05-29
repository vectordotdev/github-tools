use crate::config::Config;
use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

const GRAPHQL_URL: &str = "https://api.github.com/graphql";
const PAGE_SIZE: u32 = 100;

const ISSUES_QUERY: &str = r#"
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    issues(first: $first, after: $after, orderBy: {field: CREATED_AT, direction: DESC}) {
      pageInfo {
        endCursor
        hasNextPage
      }
      nodes {
        databaseId
        number
        title
        state
        createdAt
        updatedAt
        closedAt
        author {
          login
        }
        issueType {
          name
        }
        labels(first: 100) {
          nodes {
            id
            name
            color
            description
          }
        }
      }
    }
  }
}
"#;

const ISSUES_SINCE_QUERY: &str = r#"
query($owner: String!, $name: String!, $first: Int!, $after: String, $since: DateTime!) {
  repository(owner: $owner, name: $name) {
    issues(first: $first, after: $after, filterBy: {since: $since}, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo {
        endCursor
        hasNextPage
      }
      nodes {
        databaseId
        number
        title
        state
        createdAt
        updatedAt
        closedAt
        author {
          login
        }
        issueType {
          name
        }
        labels(first: 100) {
          nodes {
            id
            name
            color
            description
          }
        }
      }
    }
  }
}
"#;

const PRS_QUERY: &str = r#"
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: $first, after: $after, orderBy: {field: CREATED_AT, direction: DESC}) {
      pageInfo {
        endCursor
        hasNextPage
      }
      nodes {
        databaseId
        number
        title
        state
        createdAt
        updatedAt
        closedAt
        isDraft
        author {
          login
        }
        labels(first: 100) {
          nodes {
            id
            name
            color
            description
          }
        }
      }
    }
  }
}
"#;

pub fn run(config: &Config, since: Option<&str>) -> Result<()> {
    run_with_client(&Client::new(), config, since)
}

pub fn run_with_client(client: &Client, config: &Config, since: Option<&str>) -> Result<()> {
    let since_ts = since.map(parse_since).transpose()?;
    let mut all_items = Vec::new();

    println!("Fetching issues via GraphQL{}...",
        since_ts.as_ref().map(|s| format!(" (since {s})")).unwrap_or_default());
    let issues = if let Some(ref ts) = since_ts {
        fetch_paginated_since(client, config, ISSUES_SINCE_QUERY, "issues", ts)?
    } else {
        fetch_paginated(client, config, ISSUES_QUERY, "issues")?
    };
    println!("Total issues fetched: {}", issues.len());
    for node in issues {
        all_items.push(to_rest_format(&node, false));
    }

    println!("Fetching pull requests via GraphQL{}...",
        since_ts.as_ref().map(|s| format!(" (since {s})")).unwrap_or_default());
    let prs = if let Some(ref ts) = since_ts {
        fetch_paginated_since_prs(client, config, PRS_QUERY, "pullRequests", ts)?
    } else {
        fetch_paginated(client, config, PRS_QUERY, "pullRequests")?
    };
    println!("Total pull requests fetched: {}", prs.len());
    for node in prs {
        all_items.push(to_rest_format(&node, true));
    }

    println!("Total issues/PRs fetched: {}", all_items.len());

    let repo_prefix = format!("{}_{}", config.repo_owner, config.repo_name);
    let out_dir = Path::new("data").join(&repo_prefix).join("issues");
    fs::create_dir_all(&out_dir)?;
    let written = write_year_bucketed(&all_items, &out_dir, "created_at")?;
    for (year, count) in &written {
        println!("  {year}.json: wrote {count} items");
    }

    Ok(())
}

/// Parse a --since value: ISO date (2026-01-01), YYYY-MM (2026-01), or relative (3m, 1y, 30d).
pub fn parse_since(input: &str) -> Result<String> {
    // Relative: e.g. "3m", "1y", "30d"
    if let Some(num_str) = input.strip_suffix('d') {
        let days: i64 = num_str.parse().context("Invalid number in relative date")?;
        let dt = Utc::now() - chrono::Duration::days(days);
        return Ok(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }
    if let Some(num_str) = input.strip_suffix('m') {
        let months: i32 = num_str.parse().context("Invalid number in relative date")?;
        let now = Utc::now();
        let total = now.format("%Y").to_string().parse::<i32>().unwrap() * 12
            + now.format("%m").to_string().parse::<i32>().unwrap()
            - 1
            - months;
        let y = total / 12;
        let m = total % 12 + 1;
        return Ok(format!("{y}-{m:02}-01T00:00:00Z"));
    }
    if let Some(num_str) = input.strip_suffix('y') {
        let years: i32 = num_str.parse().context("Invalid number in relative date")?;
        let now = Utc::now();
        let y: i32 = now.format("%Y").to_string().parse::<i32>().unwrap() - years;
        let m = now.format("%m").to_string();
        return Ok(format!("{y}-{m}-01T00:00:00Z"));
    }
    // YYYY-MM
    if input.len() == 7 && input.chars().nth(4) == Some('-') {
        return Ok(format!("{input}-01T00:00:00Z"));
    }
    // YYYY-MM-DD
    if input.len() == 10 {
        return Ok(format!("{input}T00:00:00Z"));
    }
    // Already ISO
    Ok(input.to_string())
}

/// Append items to year-bucketed JSON files based on a date field.
fn write_year_bucketed(items: &[Value], out_dir: &Path, date_field: &str) -> Result<BTreeMap<String, usize>> {
    let mut by_year: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for item in items {
        let year = item[date_field]
            .as_str()
            .unwrap_or("")
            .get(..4)
            .unwrap_or("unknown")
            .to_string();
        by_year.entry(year).or_default().push(item.clone());
    }

    let mut counts = BTreeMap::new();
    for (year, new_items) in &by_year {
        let path = out_dir.join(format!("{year}.json"));

        // Merge: existing items keyed by number; fresh fetch wins for duplicates.
        let mut by_number: std::collections::HashMap<u64, Value> = if path.exists() {
            let json = fs::read_to_string(&path)?;
            let existing: Vec<Value> = serde_json::from_str(&json).unwrap_or_default();
            existing.into_iter()
                .filter_map(|v| v["number"].as_u64().map(|n| (n, v)))
                .collect()
        } else {
            std::collections::HashMap::new()
        };

        for item in new_items {
            if let Some(n) = item["number"].as_u64() {
                by_number.insert(n, item.clone());
            }
        }

        let mut merged: Vec<Value> = by_number.into_values().collect();
        merged.sort_by_key(|v| v["number"].as_u64().unwrap_or(0));

        let json_str = serde_json::to_string_pretty(&merged)?;
        fs::write(&path, json_str)?;
        counts.insert(year.clone(), new_items.len());
    }
    Ok(counts)
}

fn fetch_paginated(
    client: &Client,
    config: &Config,
    query: &str,
    field_name: &str,
) -> Result<Vec<Value>> {
    let mut all_nodes = Vec::new();
    let mut after: Option<String> = None;
    let mut page = 1;

    loop {
        println!("Fetching page {page} (batch size: {PAGE_SIZE}, type: {field_name})...");

        let body = json!({
            "query": query,
            "variables": {
                "owner": config.repo_owner,
                "name": config.repo_name,
                "first": PAGE_SIZE,
                "after": after,
            }
        });

        let response = client
            .post(GRAPHQL_URL)
            .header("Authorization", format!("Bearer {}", config.github_token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "github-tools")
            .json(&body)
            .send()
            .context("Failed to send GraphQL request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            eprintln!("Warning: GraphQL request failed ({status}): {text}");
            break;
        }

        let result: Value = response.json().context("Failed to parse GraphQL response")?;

        if let Some(errors) = result.get("errors") {
            eprintln!("Warning: GraphQL errors: {errors}");
            break;
        }

        let connection = &result["data"]["repository"][field_name];
        let nodes = connection["nodes"]
            .as_array()
            .context("Missing nodes in GraphQL response")?;
        let has_next = connection["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false);
        let end_cursor = connection["pageInfo"]["endCursor"]
            .as_str()
            .map(|s| s.to_string());

        all_nodes.extend(nodes.iter().cloned());
        println!(
            "Page {page} fetched. Total collected: {}",
            all_nodes.len()
        );

        if !has_next {
            println!("Reached the last page.");
            break;
        }
        after = end_cursor;
        page += 1;
    }

    Ok(all_nodes)
}

/// Like fetch_paginated but passes a $since variable for issues filterBy.
fn fetch_paginated_since(
    client: &Client,
    config: &Config,
    query: &str,
    field_name: &str,
    since: &str,
) -> Result<Vec<Value>> {
    let mut all_nodes = Vec::new();
    let mut after: Option<String> = None;
    let mut page = 1;

    loop {
        println!("Fetching page {page} (batch size: {PAGE_SIZE}, type: {field_name})...");

        let body = json!({
            "query": query,
            "variables": {
                "owner": config.repo_owner,
                "name": config.repo_name,
                "first": PAGE_SIZE,
                "after": after,
                "since": since,
            }
        });

        let response = client
            .post(GRAPHQL_URL)
            .header("Authorization", format!("Bearer {}", config.github_token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "github-tools")
            .json(&body)
            .send()
            .context("Failed to send GraphQL request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            eprintln!("Warning: GraphQL request failed ({status}): {text}");
            break;
        }

        let result: Value = response.json().context("Failed to parse GraphQL response")?;

        if let Some(errors) = result.get("errors") {
            eprintln!("Warning: GraphQL errors: {errors}");
            break;
        }

        let connection = &result["data"]["repository"][field_name];
        let nodes = connection["nodes"]
            .as_array()
            .context("Missing nodes in GraphQL response")?;
        let has_next = connection["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false);
        let end_cursor = connection["pageInfo"]["endCursor"]
            .as_str()
            .map(|s| s.to_string());

        all_nodes.extend(nodes.iter().cloned());
        println!(
            "Page {page} fetched. Total collected: {}",
            all_nodes.len()
        );

        if !has_next {
            println!("Reached the last page.");
            break;
        }
        after = end_cursor;
        page += 1;
    }

    Ok(all_nodes)
}

/// For PRs (no filterBy support): order by UPDATED_AT DESC and stop when we hit items older than since.
fn fetch_paginated_since_prs(
    client: &Client,
    config: &Config,
    query: &str,
    field_name: &str,
    since: &str,
) -> Result<Vec<Value>> {
    // Swap orderBy to UPDATED_AT for since mode
    let query = query.replace(
        "orderBy: {field: CREATED_AT, direction: DESC}",
        "orderBy: {field: UPDATED_AT, direction: DESC}",
    );
    let mut all_nodes = Vec::new();
    let mut after: Option<String> = None;
    let mut page = 1;

    loop {
        println!("Fetching page {page} (batch size: {PAGE_SIZE}, type: {field_name})...");

        let body = json!({
            "query": query,
            "variables": {
                "owner": config.repo_owner,
                "name": config.repo_name,
                "first": PAGE_SIZE,
                "after": after,
            }
        });

        let response = client
            .post(GRAPHQL_URL)
            .header("Authorization", format!("Bearer {}", config.github_token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "github-tools")
            .json(&body)
            .send()
            .context("Failed to send GraphQL request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            eprintln!("Warning: GraphQL request failed ({status}): {text}");
            break;
        }

        let result: Value = response.json().context("Failed to parse GraphQL response")?;

        if let Some(errors) = result.get("errors") {
            eprintln!("Warning: GraphQL errors: {errors}");
            break;
        }

        let connection = &result["data"]["repository"][field_name];
        let nodes = connection["nodes"]
            .as_array()
            .context("Missing nodes in GraphQL response")?;
        let has_next = connection["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false);
        let end_cursor = connection["pageInfo"]["endCursor"]
            .as_str()
            .map(|s| s.to_string());

        // Check if any node has updatedAt < since — if so, we've gone past the window
        let mut hit_boundary = false;
        for node in nodes {
            let updated = node["updatedAt"].as_str().unwrap_or("");
            if updated < since {
                hit_boundary = true;
            } else {
                all_nodes.push(node.clone());
            }
        }

        println!(
            "Page {page} fetched. Total collected: {}",
            all_nodes.len()
        );

        if hit_boundary || !has_next {
            if hit_boundary {
                println!("Reached items older than --since cutoff.");
            } else {
                println!("Reached the last page.");
            }
            break;
        }
        after = end_cursor;
        page += 1;
    }

    Ok(all_nodes)
}

/// Convert a GraphQL node into the REST API JSON shape that build_db.rs expects.
fn to_rest_format(node: &Value, is_pr: bool) -> Value {
    let labels: Vec<Value> = node["labels"]["nodes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|l| {
                    json!({
                        "id": stable_id(l["id"].as_str().unwrap_or("")),
                        "name": l["name"],
                        "color": l["color"],
                        "description": l["description"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // GraphQL returns OPEN/CLOSED/MERGED; REST uses open/closed
    let state = node["state"]
        .as_str()
        .unwrap_or("open")
        .to_lowercase()
        .replace("merged", "closed");

    let issue_type = node["issueType"]["name"].as_str().map(|s| s.to_string());

    let mut item = json!({
        "id": node["databaseId"],
        "number": node["number"],
        "title": node["title"],
        "state": state,
        "created_at": node["createdAt"],
        "updated_at": node["updatedAt"],
        "closed_at": node["closedAt"],
        "user": {
            "login": node["author"]["login"],
        },
        "labels": labels,
        "issue_type": issue_type,
    });

    if is_pr {
        item["pull_request"] = json!({});
        item["draft"] = json!(node["isDraft"].as_bool().unwrap_or(false));
    }

    item
}

/// Produce a stable i64 from a GraphQL node ID string, for use as a database key.
fn stable_id(node_id: &str) -> i64 {
    let mut hasher = DefaultHasher::new();
    node_id.hash(&mut hasher);
    // Mask to positive i64
    (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as i64
}
