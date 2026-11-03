//! A complete, privacy-minimal snapshot of PRs awaiting a first human review.
use super::{graphql_fetch, sorted_json};
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use graphql_fetch::{required, string, timestamp};
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

const PRS: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: 50, after: $after, states: OPEN, orderBy: {field: CREATED_AT, direction: ASC}) {
      nodes { id number title state isDraft createdAt updatedAt author { __typename login }
        labels(first: 100) { nodes { id name } pageInfo { hasNextPage endCursor } }
        reviews(first: 100, states: [APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED]) {
          nodes { id state submittedAt author { __typename login } }
          pageInfo { hasNextPage endCursor }
        }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
}"#;
const REVIEWS: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $after: String!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviews(first: 100, after: $after, states: [APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED]) {
        nodes { id state submittedAt author { __typename login } }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}"#;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingPr {
    number: u64,
    title: String,
    created_at: String,
    updated_at: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstReviewSnapshot {
    schema_version: u8,
    repository: String,
    collected_at: String,
    open_prs: usize,
    excluded_drafts: usize,
    excluded_bot_prs: usize,
    excluded_awaiting_author_prs: usize,
    unknown_prs: usize,
    reviewed_prs: usize,
    prs: Vec<WaitingPr>,
}

enum Actor {
    Human(String),
    Bot,
    Unknown,
}
fn actor(value: &Value) -> Result<Actor> {
    if value.is_null() {
        return Ok(Actor::Unknown);
    }
    let login = string(value, "login")?.to_lowercase();
    ensure!(!login.trim().is_empty(), "Empty actor login");
    Ok(match string(value, "__typename")? {
        "Bot" => Actor::Bot,
        "User"
            if login.ends_with("[bot]")
                || [
                    "dependabot",
                    "dependabot-preview",
                    "renovate",
                    "github-actions",
                ]
                .contains(&login.as_str()) =>
        {
            Actor::Bot
        }
        "User" => Actor::Human(login),
        _ => Actor::Unknown,
    })
}

pub fn collect(
    config: &Config,
    mut fetch: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<FirstReviewSnapshot> {
    let nodes = graphql_fetch::paginate(
        "open PRs",
        None,
        |after| {
            graphql_fetch::connection(
                fetch(
                    PRS,
                    json!({"owner":config.org,"name":config.repo,"after":after}),
                )?,
                &["repository", "pullRequests"],
            )
        },
        Ok,
    )?;
    let mut snapshot = FirstReviewSnapshot {
        schema_version: 1,
        repository: format!("{}/{}", config.org, config.repo),
        collected_at: String::new(),
        open_prs: nodes.len(),
        excluded_drafts: 0,
        excluded_bot_prs: 0,
        excluded_awaiting_author_prs: 0,
        unknown_prs: 0,
        reviewed_prs: 0,
        prs: vec![],
    };
    for node in nodes {
        ensure!(string(&node, "state")? == "OPEN", "Expected an open PR");
        let created = timestamp(&node, "createdAt")?;
        let updated = timestamp(&node, "updatedAt")?;
        ensure!(
            created <= updated && updated <= Utc::now(),
            "Invalid PR dates"
        );
        let title = string(&node, "title")?;
        ensure!(!title.trim().is_empty(), "Empty PR title");
        let number = required(&node, "number")?
            .as_u64()
            .filter(|n| *n > 0)
            .context("Invalid PR number")?;
        if required(&node, "isDraft")?
            .as_bool()
            .context("Invalid draft state")?
        {
            snapshot.excluded_drafts += 1;
            continue;
        }
        let author = match actor(required(&node, "author")?)? {
            Actor::Bot => {
                snapshot.excluded_bot_prs += 1;
                continue;
            }
            Actor::Unknown => {
                snapshot.unknown_prs += 1;
                continue;
            }
            Actor::Human(login) => login,
        };
        let labels = graphql_fetch::page(required(&node, "labels")?)?;
        ensure!(labels.next_cursor.is_none(), "Incomplete PR labels");
        let mut awaiting_author = false;
        for label in labels.nodes {
            let name = string(label, "name")?.trim().to_lowercase();
            let normalized = name
                .split_once(':')
                .map(|(prefix, value)| format!("{}:{}", prefix, value.trim_start()))
                .unwrap_or(name);
            awaiting_author |= normalized == "meta:awaiting author";
        }
        if awaiting_author {
            snapshot.excluded_awaiting_author_prs += 1;
            continue;
        }
        let mut initial = Some(required(&node, "reviews")?.clone());
        let reviews = graphql_fetch::paginate(
            "submitted reviews",
            None,
            |after| {
                if let Some(page) = initial.take() {
                    return Ok(page);
                }
                graphql_fetch::connection(
                    fetch(
                        REVIEWS,
                        json!({"owner":config.org,"name":config.repo,"number":number,"after":after}),
                    )?,
                    &["repository", "pullRequest", "reviews"],
                )
            },
            |review| {
                ensure!(
                    ["APPROVED", "CHANGES_REQUESTED", "COMMENTED", "DISMISSED"]
                        .contains(&string(&review, "state")?),
                    "Unexpected review state"
                );
                let submitted = timestamp(&review, "submittedAt")?;
                ensure!(
                    submitted >= created && submitted <= Utc::now(),
                    "Invalid review date"
                );
                actor(required(&review, "author")?)
            },
        )?;
        if reviews
            .iter()
            .any(|review| matches!(review, Actor::Human(login) if login != &author))
        {
            snapshot.reviewed_prs += 1;
        } else if reviews
            .iter()
            .any(|review| matches!(review, Actor::Unknown))
        {
            snapshot.unknown_prs += 1;
        } else {
            snapshot.prs.push(WaitingPr {
                number,
                title: title.into(),
                created_at: created.to_rfc3339(),
                updated_at: updated.to_rfc3339(),
            });
        }
    }
    snapshot.prs.sort_by_key(|pr| pr.number);
    snapshot.collected_at = Utc::now().to_rfc3339();
    Ok(snapshot)
}

fn request(client: &Client, config: &Config, query: &str, variables: Value) -> Result<Value> {
    if !config.github_token.is_empty() {
        return graphql_fetch::request(client, config, query, variables);
    }
    // Reuse gh authentication without extracting its token.
    let executable = if cfg!(target_os = "macos") && Path::new("/opt/homebrew/bin/gh").exists() {
        "/opt/homebrew/bin/gh"
    } else {
        "gh"
    };
    let mut child = Command::new(executable)
        .args(["api", "graphql", "--hostname", "github.com", "--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Could not start authenticated gh")?;
    let written = child
        .stdin
        .take()
        .context("Missing gh stdin")?
        .write_all(&serde_json::to_vec(
            &json!({"query":query,"variables":variables}),
        )?);
    let output = child.wait_with_output()?;
    written?;
    ensure!(
        output.status.success(),
        "GitHub request failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        value.get("errors").is_none(),
        "GraphQL response contains errors: {}",
        value["errors"]
    );
    ensure!(value["data"].is_object(), "Missing GraphQL data");
    Ok(value["data"].clone())
}

fn save(snapshot: &FirstReviewSnapshot, directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    let temporary = directory.join("latest.json.tmp");
    fs::write(&temporary, sorted_json(snapshot)?)?;
    fs::rename(temporary, directory.join("latest.json"))?;
    Ok(())
}
pub fn run(config: &Config) -> Result<()> {
    let client = Client::new();
    run_with_fetch(
        config,
        &Path::new("data")
            .join(format!("{}_{}", config.org, config.repo))
            .join("first-review"),
        |query, variables| request(&client, config, query, variables),
    )
}
fn run_with_fetch(
    config: &Config,
    directory: &Path,
    fetch: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<()> {
    let snapshot = collect(config, fetch)?;
    println!(
        "{}/{}: {} waiting for first review; {} unknown out of {} open PRs",
        config.org,
        config.repo,
        snapshot.prs.len(),
        snapshot.unknown_prs,
        snapshot.open_prs
    );
    save(&snapshot, directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphql_fetch::test_support::{TempDir, config, page};
    use std::collections::VecDeque;

    fn user(login: &str) -> Value {
        json!({"__typename":"User", "login":login})
    }
    fn review(id: u64, author: Value, state: &str) -> Value {
        json!({"id":format!("review-{id}"),"author":author,"state":state,"submittedAt":"2026-01-02T00:00:00Z"})
    }
    fn pr(number: u64, reviews: Value) -> Value {
        json!({"id":format!("pr-{number}"),"number":number,"title":format!("PR {number}"),"state":"OPEN","isDraft":false,
            "createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-03T00:00:00Z","author":user("author"),"reviews":reviews,"labels":page(vec![],None)})
    }
    fn response(nodes: Vec<Value>, next: Option<&str>) -> Value {
        json!({"repository":{"pullRequests":page(nodes,next)}})
    }

    #[test]
    fn separates_first_review_from_bot_self_draft_and_unknown_records() {
        let bot = json!({"__typename":"Bot","login":"review-bot"});
        let mut draft = pr(3, page(vec![], None));
        draft["isDraft"] = json!(true);
        let mut bot_pr = pr(4, page(vec![], None));
        bot_pr["author"] = bot.clone();
        let mut deleted = pr(5, page(vec![], None));
        deleted["author"] = Value::Null;
        let nodes = vec![
            pr(
                1,
                page(
                    vec![
                        review(1, bot, "COMMENTED"),
                        review(2, user("AUTHOR"), "COMMENTED"),
                    ],
                    None,
                ),
            ),
            pr(
                2,
                page(vec![review(3, user("reviewer"), "CHANGES_REQUESTED")], None),
            ),
            draft,
            bot_pr,
            deleted,
            pr(6, page(vec![review(4, Value::Null, "COMMENTED")], None)),
            pr(
                7,
                page(vec![review(5, user("reviewer"), "DISMISSED")], None),
            ),
            pr(
                8,
                page(vec![review(6, user("dependabot"), "APPROVED")], None),
            ),
        ];
        let result = collect(&config(), |_, _| Ok(response(nodes.clone(), None))).unwrap();
        assert_eq!(
            result.prs.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![1, 8]
        );
        assert_eq!(
            (
                result.open_prs,
                result.excluded_drafts,
                result.excluded_bot_prs,
                result.unknown_prs,
                result.reviewed_prs
            ),
            (8, 1, 1, 2, 2)
        );
        let saved = sorted_json(&result).unwrap();
        assert!(!saved.contains("reviewer") && !saved.contains("dependabot"));
    }

    #[test]
    fn author_followup_labels_are_not_first_review_candidates() {
        for label in ["meta:awaiting author", " META: awaiting author "] {
            let mut labeled = pr(1, page(vec![], None));
            labeled["labels"] = page(vec![json!({"id":"label","name":label})], None);
            let snapshot =
                collect(&config(), |_, _| Ok(response(vec![labeled.clone()], None))).unwrap();
            assert_eq!(snapshot.excluded_awaiting_author_prs, 1);
            assert!(snapshot.prs.is_empty());
        }
        let mut truncated = pr(1, page(vec![], None));
        truncated["labels"] = page(vec![json!({"id":"label","name":"other"})], Some("next"));
        assert!(
            collect(&config(), |_, _| Ok(response(
                vec![truncated.clone()],
                None
            )))
            .is_err()
        );
    }

    #[test]
    fn reads_all_pr_and_review_pages_before_classifying() {
        let mut responses = VecDeque::from([
            response(
                vec![pr(
                    1,
                    page(
                        vec![review(1, user("bot[bot]"), "COMMENTED")],
                        Some("reviews-next"),
                    ),
                )],
                Some("prs-next"),
            ),
            response(vec![pr(2, page(vec![], None))], None),
            json!({"repository":{"pullRequest":{"reviews":page(vec![review(2,user("reviewer"),"APPROVED")],None)}}}),
        ]);
        let mut calls = Vec::new();
        let result = collect(&config(), |query, variables| {
            calls.push((query.to_owned(), variables));
            Ok(responses.pop_front().unwrap())
        })
        .unwrap();
        assert!(responses.is_empty());
        assert_eq!(calls[1].1["after"], "prs-next");
        assert_eq!(calls[2].1["after"], "reviews-next");
        assert_eq!(calls[2].1["number"], 1);
        assert_eq!(result.prs[0].number, 2);
        assert_eq!(result.reviewed_prs, 1);
    }

    #[test]
    fn failure_or_incomplete_pagination_never_replaces_a_saved_snapshot() {
        let directory = TempDir::new();
        let path = directory.path().join("latest.json");
        fs::write(&path, "previous complete snapshot").unwrap();
        let mut responses = VecDeque::from([
            Ok(response(vec![pr(1, page(vec![], None))], Some("next"))),
            Err(anyhow::anyhow!("request failed")),
        ]);
        assert!(
            run_with_fetch(&config(), directory.path(), |_, _| responses
                .pop_front()
                .unwrap())
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "previous complete snapshot"
        );
        let bad = pr(
            1,
            json!({"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":null}}),
        );
        assert!(collect(&config(), |_, _| Ok(response(vec![bad.clone()], None))).is_err());
        let mut pages = VecDeque::from([
            response(vec![pr(1, page(vec![], None))], Some("same")),
            response(vec![pr(2, page(vec![], None))], Some("same")),
        ]);
        assert!(collect(&config(), |_, _| Ok(pages.pop_front().unwrap())).is_err());
    }

    #[test]
    fn malformed_or_duplicate_records_fail_instead_of_becoming_unreviewed() {
        let good = pr(1, page(vec![], None));
        assert!(
            collect(&config(), |_, _| Ok(response(
                vec![good.clone(), good.clone()],
                None
            )))
            .is_err()
        );
        for (key, value) in [
            ("isDraft", Value::Null),
            ("createdAt", json!("invalid")),
            ("author", json!({"login":"missing-type"})),
            ("state", json!("CLOSED")),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(collect(&config(), |_, _| Ok(response(vec![bad.clone()], None))).is_err());
        }
        let pending = pr(1, page(vec![review(1, user("reviewer"), "PENDING")], None));
        assert!(collect(&config(), |_, _| Ok(response(vec![pending.clone()], None))).is_err());
    }
}
