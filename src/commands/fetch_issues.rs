use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
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

pub fn run(config: &Config) -> Result<()> {
    run_with_client(&Client::new(), config)
}

pub fn run_with_client(client: &Client, config: &Config) -> Result<()> {
    let mut all_items = Vec::new();

    println!("Fetching issues via GraphQL...");
    let issues = fetch_paginated(client, config, ISSUES_QUERY, "issues")?;
    println!("Total issues fetched: {}", issues.len());
    for node in issues {
        all_items.push(to_rest_format(&node, false));
    }

    println!("Fetching pull requests via GraphQL...");
    let prs = fetch_paginated(client, config, PRS_QUERY, "pullRequests")?;
    println!("Total pull requests fetched: {}", prs.len());
    for node in prs {
        all_items.push(to_rest_format(&node, true));
    }

    println!("Total issues/PRs fetched: {}", all_items.len());

    let out_dir = Path::new("out/historical/issues");
    fs::create_dir_all(out_dir)?;
    let out_file = out_dir.join(format!(
        "{}_{}_issues.json",
        config.repo_owner, config.repo_name
    ));
    let json_str = serde_json::to_string_pretty(&all_items)?;
    fs::write(&out_file, json_str)?;
    println!("Saved to '{}'", out_file.display());

    Ok(())
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
