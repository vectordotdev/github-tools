use super::sorted_json;
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

pub const ISSUES_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    issues(first: 50, after: $after, states: OPEN, orderBy: {field: CREATED_AT, direction: ASC}) {
      nodes { number title reactions { totalCount }
        comments(first: 100) { nodes { author { login } } pageInfo { hasNextPage endCursor } }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
}"#;
pub const COMMENTS_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $after: String!) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      comments(first: 100, after: $after) { nodes { author { login } } pageInfo { hasNextPage endCursor } }
    }
  }
}"#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection<T> {
    nodes: Vec<T>,
    page_info: PageInfo,
}
#[derive(Deserialize)]
struct Author {
    login: String,
}
#[derive(Deserialize)]
struct Comment {
    author: Option<Author>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reactions {
    total_count: u64,
}
#[derive(Deserialize)]
struct Issue {
    number: u64,
    title: String,
    reactions: Reactions,
    comments: Connection<Comment>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterestIssue {
    number: u64,
    title: String,
    reactions: u64,
    unique_commenters: usize,
    unattributed_comments: usize,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterestSnapshot {
    schema_version: u8,
    repository: String,
    collected_at: String,
    issues: Vec<InterestIssue>,
}

fn next_cursor(page: &PageInfo, seen: &mut HashSet<String>) -> Result<Option<String>> {
    if !page.has_next_page {
        return Ok(None);
    }
    let cursor = page
        .end_cursor
        .as_ref()
        .filter(|s| !s.is_empty())
        .context("Missing pagination cursor")?;
    ensure!(seen.insert(cursor.clone()), "Repeated pagination cursor");
    Ok(Some(cursor.clone()))
}

// Fetch all pages before writing: API failures must not replace a complete snapshot.
// Full open-issue scans also capture reactions that do not change issue.updatedAt.
pub fn collect(
    config: &Config,
    mut fetch: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<InterestSnapshot> {
    let mut issues = Vec::new();
    let mut after = None;
    let mut issue_cursors = HashSet::new();
    let mut numbers = HashSet::new();
    loop {
        let data = fetch(
            ISSUES_QUERY,
            json!({"owner": config.org, "name": config.repo, "after": after}),
        )?;
        let page: Connection<Issue> = serde_json::from_value(data["repository"]["issues"].clone())
            .context("Invalid issue page")?;
        for issue in page.nodes {
            ensure!(numbers.insert(issue.number), "Duplicate issue in snapshot");
            let mut authors = HashSet::new();
            let mut unattributed = 0;
            let mut comments = issue.comments;
            let mut comment_cursors = HashSet::new();
            loop {
                for comment in comments.nodes {
                    match comment.author {
                        Some(author) if !author.login.is_empty() => {
                            authors.insert(author.login.to_lowercase());
                        }
                        _ => unattributed += 1,
                    }
                }
                let Some(cursor) = next_cursor(&comments.page_info, &mut comment_cursors)? else {
                    break;
                };
                let data = fetch(
                    COMMENTS_QUERY,
                    json!({"owner": config.org, "name": config.repo, "number": issue.number, "after": cursor}),
                )?;
                comments = serde_json::from_value(data["repository"]["issue"]["comments"].clone())
                    .context("Invalid comment page")?;
            }
            issues.push(InterestIssue {
                number: issue.number,
                title: issue.title,
                reactions: issue.reactions.total_count,
                unique_commenters: authors.len(),
                unattributed_comments: unattributed,
            });
        }
        println!("Read interest for {} open issues", issues.len());
        after = next_cursor(&page.page_info, &mut issue_cursors)?;
        if after.is_none() {
            break;
        }
    }
    issues.sort_by_key(|i| i.number);
    Ok(InterestSnapshot {
        schema_version: 1,
        repository: format!("{}/{}", config.org, config.repo),
        collected_at: Utc::now().to_rfc3339(),
        issues,
    })
}

pub fn run(config: &Config) -> Result<()> {
    let client = Client::new();
    let snapshot = collect(config, |query, variables| {
        let response: Value = client
            .post("https://api.github.com/graphql")
            .bearer_auth(&config.github_token)
            .header("User-Agent", "github-tools")
            .json(&json!({"query": query, "variables": variables}))
            .send()?
            .error_for_status()?
            .json()?;
        if let Some(errors) = response.get("errors") {
            bail!("GitHub GraphQL errors: {errors}");
        }
        response
            .get("data")
            .cloned()
            .context("Missing GraphQL data")
    })?;
    let directory = Path::new("data")
        .join(format!("{}_{}", config.org, config.repo))
        .join("issue-interest");
    fs::create_dir_all(&directory)?;
    let temporary = directory.join("latest.json.tmp");
    fs::write(&temporary, sorted_json(&snapshot)?)?;
    fs::rename(&temporary, directory.join("latest.json"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            github_token: String::new(),
            org: "owner".into(),
            repo: "repo".into(),
        }
    }
    fn comments(logins: &[Option<&str>], more: bool) -> Value {
        json!({"nodes": logins.iter().map(|login| json!({"author": login.map(|login| json!({"login": login}))})).collect::<Vec<_>>(),
            "pageInfo": {"hasNextPage": more, "endCursor": if more { Some("next-comments") } else { None }}})
    }
    #[test]
    fn paginates_issues_and_comments_and_counts_every_distinct_account_once() {
        let mut calls = 0;
        let result = collect(&config(), |_, variables| {
            calls += 1;
            Ok(match calls {
                1 => json!({"repository": {"issues": {"nodes": [{"number": 9, "title": "First", "reactions": {"totalCount": 10},
                    "comments": comments(&[Some("Author"), Some("AUTHOR"), Some("bot[bot]"), None], true)}],
                    "pageInfo": {"hasNextPage": true, "endCursor": "next-issues"}}}}),
                2 => { assert_eq!(variables["number"], 9); assert_eq!(variables["after"], "next-comments");
                    json!({"repository": {"issue": {"comments": comments(&[Some("author"), Some("other")], false)}}}) },
                3 => { assert_eq!(variables["after"], "next-issues");
                    json!({"repository": {"issues": {"nodes": [{"number": 2, "title": "Second", "reactions": {"totalCount": 0}, "comments": comments(&[], false)}],
                        "pageInfo": {"hasNextPage": false, "endCursor": null}}}}) },
                _ => panic!("Unexpected request"),
            })
        }).unwrap();
        assert_eq!(calls, 3);
        assert_eq!(result.issues[0].number, 2);
        assert_eq!(result.issues[1].unique_commenters, 3);
        assert_eq!(result.issues[1].unattributed_comments, 1);
        assert_eq!(result.issues[1].reactions, 10);
    }
    #[test]
    fn fails_on_bad_or_failed_pages_instead_of_returning_partial_results() {
        assert!(collect(&config(), |_, _| Ok(json!({"repository": null}))).is_err());
        assert!(collect(&config(), |_, _| bail!("offline")).is_err());
        let mut seen = HashSet::new();
        let page = PageInfo {
            has_next_page: true,
            end_cursor: Some("same".into()),
        };
        assert!(next_cursor(&page, &mut seen).is_ok());
        assert!(next_cursor(&page, &mut seen).is_err());
        assert!(
            next_cursor(
                &PageInfo {
                    has_next_page: true,
                    end_cursor: None
                },
                &mut HashSet::new()
            )
            .is_err()
        );
    }
}
