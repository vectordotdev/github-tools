//! Aggregate PR backlog by effective repository access. Never persist the roster.
use super::{
    fetch_first_review::{self, Actor},
    graphql_fetch, sorted_json,
};
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::Path,
    process::Command,
};

const ACCESS: &str = r#"query($owner:String!,$name:String!,$after:String){repository(owner:$owner,name:$name){
  collaborators(first:100,after:$after,affiliation:ALL){edges{permission node{id login}} pageInfo{hasNextPage endCursor}}
}}"#;
const PRS: &str = r#"query($owner:String!,$name:String!,$after:String){repository(owner:$owner,name:$name){
  pullRequests(first:100,after:$after,states:OPEN){nodes{id number isDraft author{__typename login}} pageInfo{hasNextPage endCursor}}
}}"#;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Counts {
    team: usize,
    community: usize,
    bots: usize,
    unknown: usize,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Point {
    date: String,
    source_revision: Option<String>,
    counts: Option<Counts>,
    reason: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    schema_version: u8,
    repository: String,
    classified_at: String,
    points: Vec<Point>,
}
#[derive(Clone, Debug)]
struct Saved {
    date: String,
    revision: String,
    authors: Option<Vec<Value>>,
}
struct SavedYear {
    authors: Vec<Value>,
    numbers: Vec<u64>,
}

fn variables(config: &Config, after: Option<&str>) -> Value {
    json!({"owner": config.org, "name": config.repo, "after": after})
}
fn access(
    config: &Config,
    fetch: &mut impl FnMut(&str, Value) -> Result<Value>,
) -> Result<HashSet<String>> {
    let nodes = graphql_fetch::paginate(
        "repository access",
        None,
        |after| {
            let connection = graphql_fetch::connection(
                fetch(ACCESS, variables(config, after))?,
                &["repository", "collaborators"],
            )?;
            let edges = connection["edges"]
                .as_array()
                .context("Missing collaborator edges")?;
            let nodes = edges.iter().map(|edge| json!({"id": edge["node"]["id"], "login": edge["node"]["login"], "permission": edge["permission"]})).collect::<Vec<_>>();
            Ok(json!({"nodes": nodes, "pageInfo": connection["pageInfo"]}))
        },
        Ok,
    )?;
    let mut team = HashSet::new();
    for node in nodes {
        let login = graphql_fetch::string(&node, "login")?.to_lowercase();
        ensure!(!login.is_empty(), "Empty collaborator login");
        let permission = graphql_fetch::string(&node, "permission")?;
        ensure!(
            ["READ", "TRIAGE", "WRITE", "MAINTAIN", "ADMIN"].contains(&permission),
            "Unknown repository permission"
        );
        if ["WRITE", "MAINTAIN", "ADMIN"].contains(&permission) {
            team.insert(login);
        }
    }
    Ok(team)
}
fn count(authors: &[Value], team: &HashSet<String>) -> Result<Counts> {
    let mut counts = Counts::default();
    for author in authors {
        match fetch_first_review::actor(author)? {
            Actor::Bot => counts.bots += 1,
            Actor::Unknown => counts.unknown += 1,
            Actor::Human(login) if team.contains(&login) => counts.team += 1,
            Actor::Human(_) => counts.community += 1,
        }
    }
    Ok(counts)
}

fn git(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git").args(args).output()?;
    ensure!(
        output.status.success(),
        "Could not read saved history: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}
fn saved_authors(value: &Value) -> Result<Vec<Value>> {
    let mut authors = Vec::new();
    let mut numbers = HashSet::new();
    for row in value.as_array().context("Invalid saved year")? {
        let number = row["number"]
            .as_u64()
            .filter(|n| *n > 0)
            .context("Invalid saved number")?;
        ensure!(numbers.insert(number), "Duplicate saved number");
        ensure!(
            [Some("open"), Some("closed")].contains(&row["state"].as_str()),
            "Invalid saved state"
        );
        if row["state"] != "open" || row.get("pull_request").is_none() {
            continue;
        }
        if row["draft"]
            .as_bool()
            .context("Missing saved draft status")?
        {
            continue;
        }
        authors.push(row.get("user").cloned().unwrap_or(Value::Null));
    }
    Ok(authors)
}
fn history(path: &str, config: &Config) -> Result<Vec<Saved>> {
    let revisions = super::save_backlog_history::issue_revisions(config)?;
    let mut cache: HashMap<String, Option<SavedYear>> = HashMap::new();
    let mut days = BTreeMap::new();
    for (sha, date) in revisions {
        let at = DateTime::parse_from_rfc3339(&date)?
            .with_timezone(&Utc)
            .to_rfc3339();
        if days
            .get(&at[..10])
            .is_some_and(|saved: &Saved| saved.date >= at)
        {
            continue;
        }
        let tree = String::from_utf8(git(&["ls-tree", "-r", &sha, "--", path])?)?;
        let mut all = Vec::new();
        let mut complete = false;
        let mut numbers = HashSet::new();
        for entry in tree.lines() {
            let (meta, file) = entry.split_once('\t').context("Invalid saved tree")?;
            if !file.ends_with(".json") {
                continue;
            }
            let blob = meta.split_whitespace().nth(2).context("Missing blob ID")?;
            if !cache.contains_key(blob) {
                let value: Value = serde_json::from_slice(&git(&["cat-file", "blob", blob])?)?;
                // Invalid/incomplete saves remain gaps, never partial smaller totals.
                let parsed = saved_authors(&value).ok().map(|authors| {
                    let numbers = value
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| row["number"].as_u64().unwrap())
                        .collect();
                    SavedYear { authors, numbers }
                });
                cache.insert(blob.to_owned(), parsed);
            }
            let Some(year) = &cache[blob] else {
                all.clear();
                complete = false;
                break;
            };
            // Year partitions must not duplicate records, including closed records.
            if year.numbers.iter().any(|number| !numbers.insert(*number)) {
                all.clear();
                complete = false;
                break;
            }
            all.extend(year.authors.iter().cloned());
            complete = true;
        }
        days.insert(
            at[..10].to_owned(),
            Saved {
                date: at,
                revision: sha.to_owned(),
                authors: complete.then_some(all),
            },
        );
    }
    Ok(days.into_values().collect())
}

fn collect(
    config: &Config,
    saved: &[Saved],
    mut fetch: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<Snapshot> {
    // A permission/pagination failure aborts before any output is replaced.
    let team = access(config, &mut fetch)?;
    let prs = graphql_fetch::paginate(
        "open PR contributors",
        None,
        |after| {
            graphql_fetch::connection(
                fetch(PRS, variables(config, after))?,
                &["repository", "pullRequests"],
            )
        },
        Ok,
    )?;
    let mut current = Vec::new();
    let mut actors = HashMap::new();
    for pr in prs {
        let author = graphql_fetch::required(&pr, "author")?.clone();
        if let Some(login) = author["login"].as_str() {
            actors.insert(login.to_lowercase(), author.clone());
        }
        if !pr["isDraft"].as_bool().context("Invalid draft status")? {
            current.push(author);
        }
    }
    // Old saves only contain logins. Resolve their account types rather than
    // guessing that every historical login represents a human.
    let logins = saved
        .iter()
        .filter_map(|s| s.authors.as_ref())
        .flatten()
        .filter_map(|a| a["login"].as_str())
        .map(str::to_lowercase)
        .filter(|login| !actors.contains_key(login))
        .collect::<std::collections::BTreeSet<_>>();
    let logins = logins.into_iter().collect::<Vec<_>>();
    for batch in logins.chunks(30) {
        let fields = batch
            .iter()
            .enumerate()
            .map(|(i, login)| {
                Ok(format!(
                    "a{i}:repositoryOwner(login:{}){{__typename login}}",
                    serde_json::to_string(login)?
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        let result = fetch(&format!("query{{{fields}}}"), json!({}))?;
        for (i, login) in batch.iter().enumerate() {
            let value = result
                .get(format!("a{i}"))
                .context("Missing historical account lookup")?
                .clone();
            // Bot logins are not RepositoryOwner objects; retain known bot identities.
            let value = if value.is_null()
                && (login.ends_with("[bot]")
                    || [
                        "dependabot",
                        "dependabot-preview",
                        "renovate",
                        "github-actions",
                    ]
                    .contains(&login.as_str()))
            {
                json!({"__typename":"Bot","login":login})
            } else {
                value
            };
            actors.insert(login.clone(), value);
        }
    }
    let classified_at = Utc::now().to_rfc3339();
    let mut points = Vec::new();
    for saved in saved {
        let counts = saved
            .authors
            .as_ref()
            .map(|authors| {
                let resolved = authors
                    .iter()
                    .map(|a| {
                        a["login"]
                            .as_str()
                            .and_then(|l| actors.get(&l.to_lowercase()))
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect::<Vec<_>>();
                count(&resolved, &team)
            })
            .transpose()?;
        points.push(Point {
            date: saved.date.clone(),
            source_revision: Some(saved.revision.clone()),
            reason: counts
                .is_none()
                .then(|| "Incomplete saved PR snapshot".to_owned()),
            counts,
        });
    }
    // The live scan supersedes the older saved point on the same UTC day.
    points.retain(|p| p.date[..10] != classified_at[..10]);
    points.push(Point {
        date: classified_at.clone(),
        source_revision: None,
        counts: Some(count(&current, &team)?),
        reason: None,
    });
    Ok(Snapshot {
        schema_version: 1,
        repository: format!("{}/{}", config.org, config.repo),
        classified_at,
        points,
    })
}

pub fn run(config: &Config) -> Result<()> {
    let root = Path::new("data").join(format!("{}_{}", config.org, config.repo));
    let saved = history(
        root.join("issues").to_str().context("Invalid data path")?,
        config,
    )?;
    let client = Client::new();
    let snapshot = collect(config, &saved, |q, v| {
        if q.starts_with("query{") {
            fetch_first_review::request_accounts(&client, config, q)
        } else {
            fetch_first_review::request(&client, config, q, v)
        }
    })?;
    let directory = root.join("pr-contributors");
    fs::create_dir_all(&directory)?;
    let temp = directory.join("latest.json.tmp");
    fs::write(&temp, sorted_json(&snapshot)?)?;
    fs::rename(temp, directory.join("latest.json"))?;
    let counts = snapshot
        .points
        .last()
        .and_then(|p| p.counts.as_ref())
        .context("Missing current counts")?;
    println!(
        "{}: {} team, {} community, {} bots, {} unknown; {} saved dates",
        snapshot.repository,
        counts.team,
        counts.community,
        counts.bots,
        counts.unknown,
        snapshot.points.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphql_fetch::test_support::{config, page};
    fn human(login: &str) -> Value {
        json!({"__typename":"User","login":login})
    }
    #[test]
    fn counts_effective_access_case_insensitively_without_mixing_bots_or_unknowns() {
        let team = HashSet::from(["maintainer".to_owned()]);
        assert_eq!(
            count(
                &[
                    human("Maintainer"),
                    human("visitor"),
                    json!({"__typename":"Bot","login":"app"}),
                    human("dependabot"),
                    Value::Null
                ],
                &team
            )
            .unwrap(),
            Counts {
                team: 1,
                community: 1,
                bots: 2,
                unknown: 1
            }
        );
    }
    #[test]
    fn saved_counts_exclude_closed_issues_and_drafts_and_reject_incomplete_saves() {
        let rows = json!([
            {"number":1,"state":"open","user":{"login":"issue"}},
            {"number":2,"state":"closed","pull_request":{},"draft":false},
            {"number":3,"state":"open","pull_request":{},"draft":true},
            {"number":4,"state":"open","pull_request":{},"draft":false,"user":{"login":"visitor"}}
        ]);
        assert_eq!(
            saved_authors(&rows).unwrap(),
            vec![json!({"login":"visitor"})]
        );
        let mut broken = rows.clone();
        broken[3].as_object_mut().unwrap().remove("draft");
        assert!(saved_authors(&broken).is_err());
        broken = rows.clone();
        broken[3]["number"] = json!(1);
        assert!(saved_authors(&broken).is_err());
    }
    #[test]
    fn follows_access_and_pr_pages_and_preserves_gaps() {
        let saved = vec![Saved {
            date: "2026-01-01T00:00:00Z".to_owned(),
            revision: "a".repeat(40),
            authors: None,
        }];
        let mut calls = 0;
        let result=collect(&config(),&saved,|q,v| {
            calls+=1;
            if q.contains("collaborators") {
                let (login,permission,next)=if v["after"].is_null(){("writer","WRITE",Some("next"))}else{("reader","READ",None)};
                Ok(json!({"repository":{"collaborators":{"edges":[{"node":{"id":login,"login":login},"permission":permission}],"pageInfo":{"hasNextPage":next.is_some(),"endCursor":next}}}}))
            } else {
                let (number,author,next)=if v["after"].is_null(){(1,human("writer"),Some("next"))}else{(2,human("reader"),None)};
                Ok(json!({"repository":{"pullRequests":page(vec![json!({"id":number.to_string(),"number":number,"author":author,"isDraft":false})],next)}}))
            }
        }).unwrap();
        assert_eq!(calls, 4);
        assert!(result.points[0].counts.is_none());
        assert_eq!(
            result.points[1].counts,
            Some(Counts {
                team: 1,
                community: 1,
                bots: 0,
                unknown: 0
            })
        );
        let output = sorted_json(&result).unwrap();
        assert!(!output.contains("writer"));
        assert!(!output.contains("reader"));
    }
    #[test]
    fn rejects_partial_or_unknown_permissions_instead_of_assuming_community() {
        assert!(
            access(&config(), &mut |_, _| Ok(
                json!({"repository":{"collaborators":null}})
            ))
            .is_err()
        );
        assert!(access(&config(),&mut |_,_|Ok(json!({"repository":{"collaborators":{"edges":[{"node":{"id":"1","login":"person"},"permission":"NEW_ROLE"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}))).is_err());
    }

    #[test]
    fn resolves_old_account_types_and_applies_current_access_without_filling_missing_authors() {
        let saved = vec![Saved {
            date: "2026-01-01T00:00:00Z".to_owned(),
            revision: "a".repeat(40),
            authors: Some(vec![
                json!({"login":"writer"}),
                json!({"login":"ghost"}),
                json!({"login":"dependabot[bot]"}),
                Value::Null,
            ]),
        }];
        let result = collect(&config(), &saved, |q, _| {
            if q.contains("collaborators") {
                Ok(json!({"repository":{"collaborators":{"edges":[{"node":{"id":"1","login":"writer"},"permission":"MAINTAIN"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}))
            } else if q.contains("pullRequests") {
                Ok(json!({"repository":{"pullRequests":page(vec![],None)}}))
            } else {
                assert!(q.contains("repositoryOwner"));
                Ok(json!({"a0":null,"a1":null,"a2":human("Writer")}))
            }
        }).unwrap();
        assert_eq!(
            result.points[0].counts,
            Some(Counts {
                team: 1,
                community: 0,
                bots: 1,
                unknown: 2
            })
        );
        assert_eq!(
            result.points.last().unwrap().counts,
            Some(Counts::default())
        );
    }
}
