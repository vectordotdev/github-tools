use crate::commands::fetch_issues::parse_since;
use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const GRAPHQL_URL: &str = "https://api.github.com/graphql";
const PAGE_SIZE: u32 = 100;

const QUERY: &str = r#"
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    discussions(first: $first, after: $after) {
      pageInfo {
        endCursor
        hasNextPage
      }
      nodes {
        number
        title
        bodyText
        url
        createdAt
        updatedAt
        closedAt
        closed
        stateReason
        isAnswered
        locked
        author {
          login
        }
        category {
          name
        }
        comments {
          totalCount
        }
        upvoteCount
      }
    }
  }
}
"#;

const QUERY_SINCE: &str = r#"
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    discussions(first: $first, after: $after, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo {
        endCursor
        hasNextPage
      }
      nodes {
        number
        title
        bodyText
        url
        createdAt
        updatedAt
        closedAt
        closed
        stateReason
        isAnswered
        locked
        author {
          login
        }
        category {
          name
        }
        comments {
          totalCount
        }
        upvoteCount
      }
    }
  }
}
"#;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discussion {
    pub number: u64,
    pub title: String,
    pub body_text: String,
    pub url: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub closed: bool,
    pub state_reason: Option<String>,
    pub is_answered: Option<bool>,
    pub locked: bool,
    pub author: Option<Author>,
    pub category: Category,
    pub comments: Comments,
    pub upvote_count: u64,
}

#[derive(Deserialize, Serialize)]
pub struct Author {
    pub login: String,
}

#[derive(Deserialize, Serialize)]
pub struct Category {
    pub name: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comments {
    pub total_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    end_cursor: Option<String>,
    has_next_page: bool,
}

#[derive(Deserialize)]
struct DiscussionsPage {
    nodes: Vec<Discussion>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct Repository {
    discussions: DiscussionsPage,
}

#[derive(Deserialize)]
struct Data {
    repository: Repository,
}

#[derive(Deserialize)]
struct GraphQlResponse {
    data: Option<Data>,
    errors: Option<serde_json::Value>,
}

pub fn run(config: &Config, since: Option<&str>) -> Result<()> {
    run_with_client(&Client::new(), config, since)
}

pub fn run_with_client(client: &Client, config: &Config, since: Option<&str>) -> Result<()> {
    let since_ts = since.map(parse_since).transpose()?;

    let discussions = if let Some(ref ts) = since_ts {
        println!("Fetching discussions updated since {ts}...");
        fetch_discussions_since(client, config, ts)?
    } else {
        fetch_all_discussions(client, config)?
    };

    println!("Total discussions fetched: {}", discussions.len());

    let repo_prefix = format!("{}_{}", config.repo_owner, config.repo_name);
    let out_dir = Path::new("data").join(&repo_prefix).join("discussions");
    fs::create_dir_all(&out_dir)?;

    // Bucket by year based on createdAt
    let mut by_year: BTreeMap<String, Vec<Discussion>> = BTreeMap::new();
    for d in discussions {
        let year = d.created_at.get(..4).unwrap_or("unknown").to_string();
        by_year.entry(year).or_default().push(d);
    }

    for (year, items) in &by_year {
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

        for d in items {
            by_number.insert(d.number, serde_json::to_value(d).unwrap());
        }

        let mut merged: Vec<Value> = by_number.into_values().collect();
        merged.sort_by_key(|v| v["number"].as_u64().unwrap_or(0));

        let json_str = serde_json::to_string_pretty(&merged)?;
        fs::write(&path, json_str)?;
        println!("  {year}.json: wrote {} discussions", items.len());
    }

    Ok(())
}

fn fetch_discussions_since(client: &Client, config: &Config, since: &str) -> Result<Vec<Discussion>> {
    let mut discussions = Vec::new();
    let mut after: Option<String> = None;
    let mut page = 1;

    loop {
        println!("Fetching page {page} of discussions...");

        let body = json!({
            "query": QUERY_SINCE,
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

        let result: GraphQlResponse = response
            .json()
            .context("Failed to parse GraphQL response")?;

        if let Some(errors) = result.errors {
            eprintln!("Warning: GraphQL errors: {errors}");
            break;
        }

        let page_data = result
            .data
            .context("Missing 'data' in GraphQL response")?
            .repository
            .discussions;

        let has_next = page_data.page_info.has_next_page;
        after = page_data.page_info.end_cursor;

        let mut hit_boundary = false;
        for d in page_data.nodes {
            if d.updated_at.as_str() < since {
                hit_boundary = true;
            } else {
                discussions.push(d);
            }
        }

        println!("Fetched {} discussions so far...", discussions.len());

        if hit_boundary {
            println!("Reached discussions older than --since cutoff.");
            break;
        }
        if !has_next {
            break;
        }
        page += 1;
    }

    Ok(discussions)
}

fn fetch_all_discussions(client: &Client, config: &Config) -> Result<Vec<Discussion>> {
    let mut discussions = Vec::new();
    let mut after: Option<String> = None;
    let mut page = 1;

    loop {
        println!("Fetching page {page} of discussions...");

        let body = json!({
            "query": QUERY,
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

        let result: GraphQlResponse = response
            .json()
            .context("Failed to parse GraphQL response")?;

        if let Some(errors) = result.errors {
            eprintln!("Warning: GraphQL errors: {errors}");
            break;
        }

        let page_data = result
            .data
            .context("Missing 'data' in GraphQL response")?
            .repository
            .discussions;

        let has_next = page_data.page_info.has_next_page;
        after = page_data.page_info.end_cursor;
        discussions.extend(page_data.nodes);

        println!("Fetched {} discussions so far...", discussions.len());

        if !has_next {
            break;
        }
        page += 1;
    }

    Ok(discussions)
}
