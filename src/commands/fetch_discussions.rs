use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
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

pub fn run(config: &Config) -> Result<()> {
    run_with_client(&Client::new(), config)
}

pub fn run_with_client(client: &Client, config: &Config) -> Result<()> {
    let discussions = fetch_all_discussions(client, config)?;

    println!("Total discussions fetched: {}", discussions.len());

    let out_dir = Path::new("out/historical/discussions");
    fs::create_dir_all(out_dir)?;
    let out_file = out_dir.join(format!(
        "{}_{}_discussions.json",
        config.repo_owner, config.repo_name
    ));
    let json = serde_json::to_string_pretty(&discussions)?;
    fs::write(&out_file, json)?;
    println!("Saved to '{}'", out_file.display());

    Ok(())
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
