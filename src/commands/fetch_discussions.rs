use super::sorted_json;
use crate::commands::fetch_issues::parse_since;
use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Deserializer, Serialize};
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
        answerChosenAt
        locked
        author {
          login
        }
        category {
          isAnswerable
          name
        }
        labels(first: 100) {
          nodes { name }
          pageInfo { hasNextPage }
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
        answerChosenAt
        locked
        author {
          login
        }
        category {
          isAnswerable
          name
        }
        labels(first: 100) {
          nodes { name }
          pageInfo { hasNextPage }
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
    pub answer_chosen_at: Option<String>,
    pub locked: bool,
    pub author: Option<Author>,
    pub category: Category,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_labels")]
    pub labels: Option<Vec<String>>,
    pub comments: Comments,
    pub upvote_count: u64,
}

#[derive(Deserialize, Serialize)]
pub struct Author {
    pub login: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub name: String,
    pub is_answerable: Option<bool>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comments {
    pub total_count: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LabelInput {
    Saved(Vec<String>),
    Connection {
        nodes: Vec<Label>,
        #[serde(rename = "pageInfo")]
        page_info: LabelPageInfo,
    },
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LabelPageInfo {
    has_next_page: bool,
}

fn deserialize_labels<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Option<Vec<String>>, D::Error> {
    match Option::<LabelInput>::deserialize(deserializer)? {
        None => Ok(None),
        Some(LabelInput::Saved(names)) => Ok(Some(names)),
        Some(LabelInput::Connection { nodes, page_info }) => {
            if page_info.has_next_page {
                return Err(serde::de::Error::custom("Incomplete discussion labels"));
            }
            Ok(Some(nodes.into_iter().map(|label| label.name).collect()))
        }
    }
}

fn validate_labels(discussions: &mut [Discussion]) -> Result<()> {
    for discussion in discussions {
        let labels = discussion.labels.as_mut().with_context(|| {
            format!("Missing labels for discussion #{}", discussion.number)
        })?;
        anyhow::ensure!(
            labels.iter().all(|label| !label.trim().is_empty()),
            "Invalid label for discussion #{}",
            discussion.number
        );
        labels.sort();
    }
    Ok(())
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

    let mut discussions = if let Some(ref ts) = since_ts {
        println!("Fetching discussions updated since {ts}...");
        fetch_discussions_since(client, config, ts)?
    } else {
        fetch_all_discussions(client, config)?
    };
    validate_labels(&mut discussions)?;

    println!("Total discussions fetched: {}", discussions.len());

    let repo_prefix = format!("{}_{}", config.org, config.repo);
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

        let json_str = sorted_json(&merged)?;
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
                "owner": config.org,
                "name": config.repo,
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
                "owner": config.org,
                "name": config.repo,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_dates_round_trip_and_older_snapshots_remain_readable() {
        let mut row = json!({
            "number": 1, "title": "Question", "bodyText": "", "url": "https://github.com/example/discussions/1",
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-03T00:00:00Z",
            "closedAt": null, "closed": false, "stateReason": null, "isAnswered": true,
            "locked": false, "author": null, "category": {"name": "Q&A", "isAnswerable": true},
            "comments": {"totalCount": 1}, "upvoteCount": 0
        });
        let old: Discussion = serde_json::from_value(row.clone()).unwrap();
        assert_eq!(old.answer_chosen_at, None);
        row["answerChosenAt"] = json!("2026-01-02T00:00:00Z");
        let current: Discussion = serde_json::from_value(row.clone()).unwrap();
        assert_eq!(serde_json::to_value(current).unwrap(), row);
        assert!(QUERY.contains("answerChosenAt"));
        assert!(QUERY_SINCE.contains("answerChosenAt"));
    }

    fn discussion_with_labels(labels: Value) -> Discussion {
        serde_json::from_value(json!({
            "number": 1, "title": "Question", "bodyText": "", "url": "https://github.com/example/discussions/1",
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-03T00:00:00Z",
            "closedAt": null, "closed": false, "stateReason": null, "isAnswered": false,
            "locked": false, "author": null, "category": {"name": "Q&A", "isAnswerable": true},
            "comments": {"totalCount": 1}, "upvoteCount": 0, "labels": labels
        })).unwrap()
    }

    #[test]
    fn discussion_labels_are_complete_and_stably_ordered() {
        let mut rows = vec![discussion_with_labels(json!({
            "nodes": [{"name": "z"}, {"name": "meta:awaiting author"}],
            "pageInfo": {"hasNextPage": false}
        }))];
        validate_labels(&mut rows).unwrap();
        let saved = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(saved["labels"], json!(["meta:awaiting author", "z"]));
        let mut reloaded = vec![serde_json::from_value(saved.clone()).unwrap()];
        validate_labels(&mut reloaded).unwrap();
        assert_eq!(serde_json::to_value(&reloaded[0]).unwrap(), saved);
        let mut empty = vec![discussion_with_labels(json!({"nodes": [], "pageInfo": {"hasNextPage": false}}))];
        validate_labels(&mut empty).unwrap();
        for labels in [Value::Null,
            json!({"nodes": [{"name": " "}], "pageInfo": {"hasNextPage": false}})] {
            assert!(validate_labels(&mut [discussion_with_labels(labels)]).is_err());
        }
        let mut truncated = serde_json::to_value(&empty[0]).unwrap();
        truncated["labels"] = json!({"nodes": [], "pageInfo": {"hasNextPage": true}});
        assert!(serde_json::from_value::<Discussion>(truncated).is_err());
        assert!(QUERY.contains("labels(first: 100)"));
        assert!(QUERY_SINCE.contains("labels(first: 100)"));
    }

}
