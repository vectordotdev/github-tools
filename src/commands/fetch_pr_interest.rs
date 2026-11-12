//! Saved PR engagement plus the engagement of explicitly linked closing issues.
use super::{
    graphql_fetch as gql,
    interest::{Commenters, Engagement},
    snapshots,
};
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};

const ISSUE: &str = r#"fragment InterestIssue on Issue {
  id issueNumber: number title state repository { nameWithOwner isPrivate }
  reactions { totalCount }
  comments(first:100) { nodes { id author { login } } pageInfo { hasNextPage endCursor } }
}"#;
const PRS: &str = r#"query($owner:String!,$name:String!,$after:String) {
  repository(owner:$owner,name:$name) {
    pullRequests(first:25,after:$after,states:OPEN,orderBy:{field:CREATED_AT,direction:ASC}) {
      nodes { id number title state isDraft updatedAt reactions { totalCount }
        comments(first:100) { nodes { id author { login } } pageInfo { hasNextPage endCursor } }
        closingIssuesReferences(first:20) { nodes { ...InterestIssue } pageInfo { hasNextPage endCursor } }
      } pageInfo { hasNextPage endCursor }
    }
  }
}"#;
const LINKS: &str = r#"query($id:ID!,$after:String) {
  node(id:$id) { ... on PullRequest {
    closingIssuesReferences(first:20,after:$after) { nodes { ...InterestIssue } pageInfo { hasNextPage endCursor } }
  } }
}"#;
const COMMENTS: &str = r#"query($id:ID!,$after:String) {
  node(id:$id) {
    ... on Issue { comments(first:100,after:$after) { nodes { id author { login } } pageInfo { hasNextPage endCursor } } }
    ... on PullRequest { comments(first:100,after:$after) { nodes { id author { login } } pageInfo { hasNextPage endCursor } } }
  }
}"#;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkedIssue {
    repository: String,
    number: u64,
    title: String,
    state: String,
    #[serde(flatten)]
    engagement: Engagement,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InterestPr {
    number: u64,
    title: String,
    updated_at: String,
    #[serde(flatten)]
    engagement: Engagement,
    linked_issues: Vec<LinkedIssue>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    schema_version: u8,
    repository: String,
    collected_at: String,
    open_prs: usize,
    excluded_drafts: usize,
    prs: Vec<InterestPr>,
}
fn number(node: &Value, field: &str) -> Result<u64> {
    gql::required(node, field)?
        .as_u64()
        .filter(|n| *n > 0)
        .context("Invalid record number")
}
fn title(node: &Value) -> Result<String> {
    let title = gql::string(node, "title")?;
    ensure!(!title.trim().is_empty(), "Empty title");
    Ok(title.to_owned())
}
// Reuse the embedded first page, then strictly paginate every continuation.
fn nested(
    first: Value,
    mut fetch: impl FnMut(Option<&str>) -> Result<Value>,
) -> Result<Vec<Value>> {
    let mut first = Some(first);
    gql::paginate(
        "PR interest details",
        None,
        |after| match first.take() {
            Some(page) => Ok(page),
            None => fetch(after),
        },
        Ok,
    )
}
fn engagement(
    node: &Value,
    fetch: &mut impl FnMut(&str, Value) -> Result<Value>,
) -> Result<Engagement> {
    let reactions = gql::required(gql::required(node, "reactions")?, "totalCount")?
        .as_u64()
        .context("Invalid reaction count")?;
    let comments = nested(gql::required(node, "comments")?.clone(), |after| {
        gql::connection(
            fetch(
                COMMENTS,
                json!({"id":gql::string(node,"id")?,"after":after}),
            )?,
            &["node", "comments"],
        )
    })?;
    let mut commenters = Commenters::default();
    for comment in comments {
        let author = gql::required(&comment, "author")?;
        if author.is_null() {
            commenters.add(None);
        } else {
            let login = gql::string(author, "login")?;
            ensure!(!login.trim().is_empty(), "Empty commenter login");
            commenters.add(Some(login));
        }
    }
    Ok(commenters.finish(reactions))
}
fn collect(
    config: &Config,
    mut fetch: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<Snapshot> {
    let query = format!("{PRS}\n{ISSUE}");
    let nodes = gql::paginate(
        "open PR interest",
        None,
        |after| {
            gql::connection(
                fetch(
                    &query,
                    json!({"owner":config.org,"name":config.repo,"after":after}),
                )?,
                &["repository", "pullRequests"],
            )
        },
        Ok,
    )?;
    let mut snapshot = Snapshot {
        schema_version: 1,
        repository: format!("{}/{}", config.org, config.repo),
        collected_at: String::new(),
        open_prs: nodes.len(),
        excluded_drafts: 0,
        prs: vec![],
    };
    let mut cache = HashMap::<String, LinkedIssue>::new();
    for node in nodes {
        ensure!(gql::string(&node, "state")? == "OPEN", "Expected open PR");
        if gql::required(&node, "isDraft")?
            .as_bool()
            .context("Invalid draft flag")?
        {
            snapshot.excluded_drafts += 1;
            continue;
        }
        let updated = gql::timestamp(&node, "updatedAt")?;
        ensure!(updated <= Utc::now(), "Future PR date");
        let linked = nested(
            gql::required(&node, "closingIssuesReferences")?.clone(),
            |after| {
                gql::connection(
                    fetch(
                        &format!("{LINKS}\n{ISSUE}"),
                        json!({"id":gql::string(&node,"id")?,"after":after}),
                    )?,
                    &["node", "closingIssuesReferences"],
                )
            },
        )?;
        let mut linked_issues = vec![];
        for issue in linked {
            let id = gql::string(&issue, "id")?.to_owned();
            if !cache.contains_key(&id) {
                ensure!(
                    gql::required(gql::required(&issue, "repository")?, "isPrivate")?.as_bool()
                        == Some(false),
                    "Private or unknown linked issue repository"
                );
                let state = gql::string(&issue, "state")?;
                ensure!(
                    matches!(state, "OPEN" | "CLOSED"),
                    "Invalid linked issue state"
                );
                cache.insert(
                    id.clone(),
                    LinkedIssue {
                        repository: gql::string(
                            gql::required(&issue, "repository")?,
                            "nameWithOwner",
                        )?
                        .to_owned(),
                        number: number(&issue, "issueNumber")?,
                        title: title(&issue)?,
                        state: state.to_owned(),
                        engagement: engagement(&issue, &mut fetch)?,
                    },
                );
            }
            linked_issues.push(cache[&id].clone());
        }
        linked_issues.sort_by(|a, b| {
            a.repository
                .cmp(&b.repository)
                .then(a.number.cmp(&b.number))
        });
        snapshot.prs.push(InterestPr {
            number: number(&node, "number")?,
            title: title(&node)?,
            updated_at: updated.to_rfc3339(),
            engagement: engagement(&node, &mut fetch)?,
            linked_issues,
        });
    }
    snapshot.prs.sort_by_key(|pr| pr.number);
    snapshot.collected_at = Utc::now().to_rfc3339();
    Ok(snapshot)
}
fn run_with_fetch(
    config: &Config,
    directory: &Path,
    fetch: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<()> {
    let snapshot = collect(config, fetch)?;
    snapshots::save_latest(directory, &snapshot)?;
    println!(
        "Saved PR interest for {} non-draft PRs in {}",
        snapshot.prs.len(),
        snapshot.repository
    );
    Ok(())
}
pub fn run(config: &Config) -> Result<()> {
    let client = Client::new();
    run_with_fetch(
        config,
        &Path::new("data")
            .join(format!("{}_{}", config.org, config.repo))
            .join("pr-interest"),
        |q, v| gql::request_authenticated(&client, config, q, v),
    )
}

#[cfg(test)]
mod tests {
    use super::super::sorted_json;
    use super::gql::test_support::{TempDir, config, page};
    use super::*;
    use std::fs;
    fn comments(more: bool) -> Value {
        page(
            vec![
                json!({"id":"c1","author":{"login":"Alice"}}),
                json!({"id":"c2","author":null}),
            ],
            more.then_some("comments-2"),
        )
    }
    fn issue(repo: &str, n: u64) -> Value {
        json!({"id":format!("{repo}-{n}"),"issueNumber":n,"title":"Demand","state":"CLOSED","repository":{"nameWithOwner":repo,"isPrivate":false},
            "reactions":{"totalCount":7},"comments":comments(true)})
    }
    fn pr(n: u64, draft: bool, linked: Value) -> Value {
        json!({"id":format!("pr{n}"),"number":n,"title":"Change","state":"OPEN","isDraft":draft,"updatedAt":"2026-11-01T00:00:00Z",
            "reactions":{"totalCount":2},"comments":comments(false),"closingIssuesReferences":linked})
    }
    #[test]
    fn paginates_prs_links_and_comments_and_caches_shared_cross_repo_closed_issues() {
        let mut calls = 0;
        let result=collect(&config(),|q,v| { calls+=1; Ok(match calls {
            1=>json!({"repository":{"pullRequests":page(vec![pr(1,false,page(vec![issue("one/repo",7)],Some("links-2")))],Some("prs-2"))}}),
            2=>{ assert_eq!(v["after"],"prs-2"); json!({"repository":{"pullRequests":page(vec![pr(2,false,page(vec![issue("one/repo",7)],None)),pr(3,true,page(vec![],None))],None)}}) },
            3=>{ assert!(q.contains("node(id:$id)")); assert_eq!(v["after"],"links-2"); json!({"node":{"closingIssuesReferences":page(vec![issue("two/repo",7)],None)}}) },
            4|5=>{ assert_eq!(v["after"],"comments-2"); json!({"node":{"comments":page(vec![json!({"id":"c3","author":{"login":"ALICE"}}),json!({"id":"c4","author":{"login":"other[bot]"}})],None)}}) },
            _=>panic!("Unexpected request"),
        }) }).unwrap();
        assert_eq!(calls, 5);
        assert_eq!(result.excluded_drafts, 1);
        assert_eq!(result.prs.len(), 2);
        assert_eq!(result.prs[0].engagement.unique_commenters, 1);
        assert_eq!(result.prs[0].linked_issues.len(), 2);
        assert_eq!(
            result.prs[0].linked_issues[0].engagement.unique_commenters,
            2
        );
        assert_eq!(
            result.prs[0].linked_issues[0]
                .engagement
                .unattributed_comments,
            1
        );
        assert_eq!(result.prs[0].linked_issues[0].engagement.reactions, 7);
        assert_eq!(
            result.prs[1].linked_issues[0].engagement.unique_commenters,
            2
        );
        assert_eq!(
            serde_json::to_value(&result.prs[0]).unwrap(),
            json!({
                "number": 1, "title": "Change", "updatedAt": "2026-11-01T00:00:00+00:00",
                "reactions": 2, "uniqueCommenters": 1, "unattributedComments": 1,
                "linkedIssues": [
                    {"repository": "one/repo", "number": 7, "title": "Demand", "state": "CLOSED",
                     "reactions": 7, "uniqueCommenters": 2, "unattributedComments": 1},
                    {"repository": "two/repo", "number": 7, "title": "Demand", "state": "CLOSED",
                     "reactions": 7, "uniqueCommenters": 2, "unattributedComments": 1}
                ]
            })
        );
        let saved = sorted_json(&result).unwrap();
        assert!(
            !saved.contains("Alice")
                && !saved.contains("other[bot]")
                && !saved.contains("comments-2")
        );
    }
    #[test]
    fn empty_commenter_logins_remain_errors() {
        for login in ["", " "] {
            let mut node = pr(1, false, page(vec![], None));
            node["comments"]["nodes"][0]["author"]["login"] = json!(login);
            assert!(collect(&config(), |_, _| Ok(json!({
                "repository": {"pullRequests": page(vec![node.clone()], None)}
            }))).is_err());
        }
    }

    #[test]
    fn failed_or_invalid_nested_pages_leave_previous_snapshot_intact() {
        for bad in [
            json!(null),
            page(
                vec![{
                    let mut issue = issue("private/repo", 7);
                    issue["repository"]["isPrivate"] = json!(true);
                    issue
                }],
                None,
            ),
            page(vec![issue("one/repo", 7), issue("one/repo", 7)], None),
        ] {
            let dir = TempDir::new();
            let path = dir.path().join("latest.json");
            fs::write(&path, "previous").unwrap();
            assert!(
                run_with_fetch(&config(), dir.path(), |_, _| Ok(
                    json!({"repository":{"pullRequests":page(vec![pr(1,false,bad.clone())],None)}})
                ))
                .is_err()
            );
            assert_eq!(fs::read_to_string(path).unwrap(), "previous");
        }
        let dir = TempDir::new();
        let path = dir.path().join("latest.json");
        fs::write(&path, "previous").unwrap();
        let mut calls = 0;
        assert!(run_with_fetch(&config(),dir.path(),|_,_|{calls+=1; if calls>1 {anyhow::bail!("offline")}; Ok(json!({"repository":{"pullRequests":page(vec![pr(1,false,page(vec![issue("one/repo",7)],None))],None)}}))}).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "previous");
    }
}
