use super::{graphql_fetch, sorted_json};
use crate::commands::fetch_issues::parse_since;
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use graphql_fetch::{PAGE_SIZE, nullable_timestamp, required, string, timestamp};
use reqwest::blocking::Client;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const QUERY: &str = r#"
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    discussions(first: $first, after: $after, orderBy: {field: CREATED_AT, direction: DESC}) {
      pageInfo {
        endCursor
        hasNextPage
      }
      nodes {
        id
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_labels"
    )]
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

fn deserialize_labels<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error> {
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
        let labels = discussion
            .labels
            .as_mut()
            .with_context(|| format!("Missing labels for discussion #{}", discussion.number))?;
        anyhow::ensure!(
            labels.iter().all(|label| !label.trim().is_empty()),
            "Invalid label for discussion #{}",
            discussion.number
        );
        labels.sort();
    }
    Ok(())
}

pub fn run(config: &Config, since: Option<&str>) -> Result<()> {
    run_with_client(&Client::new(), config, since)
}

pub fn run_with_client(client: &Client, config: &Config, since: Option<&str>) -> Result<()> {
    let out_dir = Path::new("data")
        .join(format!("{}_{}", config.org, config.repo))
        .join("discussions");
    run_with_request(config, since, &out_dir, |query, variables| {
        graphql_fetch::request(client, config, query, variables)
    })
}

fn run_with_request(
    config: &Config,
    since: Option<&str>,
    out_dir: &Path,
    mut request: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<()> {
    let since_ts = since.map(parse_since).transpose()?;
    let query = QUERY;
    let scope = format!("{}/{} discussions", config.org, config.repo);
    let discussions = graphql_fetch::paginate(
        &scope,
        since_ts.as_deref(),
        |after| {
            let data = request(
                query,
                json!({"owner": config.org, "name": config.repo, "first": PAGE_SIZE, "after": after}),
            )?;
            graphql_fetch::connection(data, &["repository", "discussions"])
        },
        decode_discussion,
    )?;

    println!("Total discussions fetched: {}", discussions.len());

    fs::create_dir_all(out_dir)?;

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
            existing
                .into_iter()
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

fn decode_discussion(node: Value) -> Result<Discussion> {
    timestamp(&node, "createdAt")?;
    timestamp(&node, "updatedAt")?;
    for field in ["closedAt", "answerChosenAt"] {
        nullable_timestamp(&node, field)?;
    }
    // API-selected nullable fields must be present. Saved snapshots keep their older
    // optional-field compatibility through Discussion's existing Deserialize impl.
    for field in ["stateReason", "isAnswered", "author"] {
        required(&node, field)?;
    }
    required(required(&node, "category")?, "isAnswerable")?
        .as_bool()
        .context("Invalid discussion category isAnswerable")?;
    let reason = required(&node, "stateReason")?;
    ensure!(
        reason.is_null()
            || matches!(
                reason.as_str(),
                Some("RESOLVED" | "OUTDATED" | "DUPLICATE" | "REOPENED")
            ),
        "Invalid discussion stateReason"
    );
    reqwest::Url::parse(string(&node, "url")?).context("Invalid discussion URL")?;
    ensure!(
        required(&node, "labels")?.is_object(),
        "Invalid discussion labels connection"
    );
    let mut discussion: Discussion =
        serde_json::from_value(node).context("Invalid discussion record")?;
    ensure!(discussion.number > 0, "Invalid discussion number");
    validate_labels(std::slice::from_mut(&mut discussion))?;
    Ok(discussion)
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
        assert_eq!(old.labels, None);
        row["answerChosenAt"] = json!("2026-01-02T00:00:00Z");
        let current: Discussion = serde_json::from_value(row.clone()).unwrap();
        assert_eq!(serde_json::to_value(current).unwrap(), row);
        assert!(QUERY.contains("answerChosenAt"));
        assert!(QUERY.contains("CREATED_AT"));
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
        let mut empty = vec![discussion_with_labels(
            json!({"nodes": [], "pageInfo": {"hasNextPage": false}}),
        )];
        validate_labels(&mut empty).unwrap();
        for labels in [
            Value::Null,
            json!({"nodes": [{"name": " "}], "pageInfo": {"hasNextPage": false}}),
        ] {
            assert!(validate_labels(&mut [discussion_with_labels(labels)]).is_err());
        }
        let mut truncated = serde_json::to_value(&empty[0]).unwrap();
        truncated["labels"] = json!({"nodes": [], "pageInfo": {"hasNextPage": true}});
        assert!(serde_json::from_value::<Discussion>(truncated).is_err());
        assert!(QUERY.contains("labels(first: 100)"));
        assert!(QUERY.contains("CREATED_AT"));
    }
}

#[cfg(test)]
mod fetch_safety_tests {
    use super::*;
    use crate::commands::graphql_fetch::test_support::{Server, TempDir, client, config, ok, page};

    fn node(number: u64) -> Value {
        json!({"id": format!("D{number}"), "number": number, "title": "Question", "bodyText": "",
            "url": format!("https://github.com/example/project/discussions/{number}"),
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-03-01T00:00:00Z",
            "closedAt": null, "closed": true, "stateReason": "RESOLVED", "isAnswered": null,
            "answerChosenAt": null, "locked": false, "author": null,
            "category": {"name": "Q&A", "isAnswerable": true},
            "labels": {"nodes": [], "pageInfo": {"hasNextPage": false}},
            "comments": {"totalCount": 0}, "upvoteCount": 0})
    }
    fn data(nodes: Vec<Value>, next: Option<&str>) -> Value {
        json!({"repository": {"discussions": page(nodes, next)}})
    }

    #[test]
    fn later_discussion_page_failures_preserve_all_year_files_in_both_modes() {
        for since in [None, Some("2026-01-01")] {
            let mut invalid = node(2);
            invalid["isAnswered"] = json!("unknown");
            for (failure, marker) in [
                (ok(data(vec![invalid], None)), "Invalid discussion record"),
                ((502, json!({"data": {}}).to_string()), "502"),
                ((200, json!({"data": {"repository": {}}, "errors": [{"message": "fixture failure"}]}).to_string()), "fixture failure"),
                ((200, "{".into()), "Malformed GitHub GraphQL JSON"),
            ] {
                let dir = TempDir::new();
                let before = [("2025.json", b"[{\"number\":80,\"retained\":true}]".to_vec()), ("2026.json", b"[{\"number\":90,\"retained\":true}]".to_vec())];
                for (name, bytes) in &before { fs::write(dir.path().join(name), bytes).unwrap(); }
                let server = Server::new(vec![ok(data(vec![node(1)], Some("next-discussions"))), failure]);
                let cfg = config();
                let client = client();
                let error = run_with_request(&cfg, since, dir.path(), |query, variables| graphql_fetch::request_at(&client, &cfg, &server.url, query, variables)).unwrap_err();
                let message = format!("{error:#}");
                assert!(message.contains("discussions, page 2"), "{message}");
                assert!(message.contains(marker), "{message}");
                for (name, bytes) in before { assert_eq!(fs::read(dir.path().join(name)).unwrap(), bytes); }
                assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
                let requests = server.finish();
                assert_eq!(requests.len(), 2);
                assert!(requests[0]["variables"]["after"].is_null());
                assert_eq!(requests[1]["variables"]["after"], "next-discussions");
            }
        }
    }

    #[test]
    fn malformed_discussion_labels_preserve_all_year_files_in_both_modes() {
        for since in [None, Some("2026-01-01")] {
            let mut failures = vec![None, Some(Value::Null), Some(json!([]))];
            failures.extend(
                [
                    json!({"nodes": [], "pageInfo": {"hasNextPage": true}}),
                    json!({"nodes": [], "pageInfo": {}}),
                    json!({"nodes": [], "pageInfo": {"hasNextPage": null}}),
                    json!({"pageInfo": {"hasNextPage": false}}),
                    json!({"nodes": [null], "pageInfo": {"hasNextPage": false}}),
                    json!({"nodes": [{"name": " "}], "pageInfo": {"hasNextPage": false}}),
                ]
                .into_iter()
                .map(Some),
            );
            for labels in failures {
                let dir = TempDir::new();
                let before = [
                    ("2025.json", b"[{\"number\":80,\"retained\":true}]".to_vec()),
                    ("2026.json", b"[{\"number\":90,\"retained\":true}]".to_vec()),
                ];
                for (name, bytes) in &before {
                    fs::write(dir.path().join(name), bytes).unwrap();
                }
                let mut invalid = node(2);
                // Invalid records must be rejected even when outside the cutoff.
                invalid["updatedAt"] = json!("2025-12-31T00:00:00Z");
                if let Some(labels) = labels {
                    invalid["labels"] = labels;
                } else {
                    invalid.as_object_mut().unwrap().remove("labels");
                }
                let server = Server::new(vec![
                    ok(data(vec![node(1)], Some("next-discussions"))),
                    ok(data(vec![invalid], None)),
                ]);
                let cfg = config();
                let client = client();
                let error = run_with_request(&cfg, since, dir.path(), |query, variables| {
                    graphql_fetch::request_at(&client, &cfg, &server.url, query, variables)
                })
                .unwrap_err();
                let message = format!("{error:#}");
                assert!(
                    message.contains("discussions, page 2, node D2"),
                    "{message}"
                );
                for (name, bytes) in before {
                    assert_eq!(fs::read(dir.path().join(name)).unwrap(), bytes);
                }
                assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
                assert_eq!(server.finish().len(), 2);
            }
        }
    }

    #[test]
    fn healthy_full_and_incremental_discussions_keep_nullable_metadata_and_snapshot_shape() {
        for since in [None, Some("2026-01-01")] {
            let dir = TempDir::new();
            let mut answered = node(2);
            answered["labels"] = json!({
                "nodes": [{"name": "z"}, {"name": "meta:awaiting author"}],
                "pageInfo": {"hasNextPage": false}
            });
            answered["isAnswered"] = json!(true);
            answered["answerChosenAt"] = json!("2026-02-01T00:00:00Z");
            let server = Server::new(vec![
                ok(data(vec![node(1)], Some("next"))),
                ok(data(vec![answered], None)),
            ]);
            let cfg = config();
            let client = client();
            run_with_request(&cfg, since, dir.path(), |query, variables| {
                graphql_fetch::request_at(&client, &cfg, &server.url, query, variables)
            })
            .unwrap();
            let saved: Vec<Value> =
                serde_json::from_slice(&fs::read(dir.path().join("2026.json")).unwrap()).unwrap();
            assert_eq!(saved.len(), 2);
            assert!(saved[0]["isAnswered"].is_null());
            assert!(saved[0]["closedAt"].is_null());
            assert!(saved[0]["author"].is_null());
            assert_eq!(saved[1]["answerChosenAt"], "2026-02-01T00:00:00Z");
            assert_eq!(saved[0]["labels"], json!([]));
            assert_eq!(saved[1]["labels"], json!(["meta:awaiting author", "z"]));
            assert_eq!(saved[0].get("id"), None);
            let requests = server.finish();
            assert_eq!(requests[1]["variables"]["after"], "next");
            assert!(
                requests[0]["query"]
                    .as_str()
                    .unwrap()
                    .contains("field: CREATED_AT")
            );
        }
    }

    #[test]
    fn malformed_discussion_fields_fail_while_explicit_nullable_values_remain_valid() {
        let valid = node(1);
        assert!(decode_discussion(valid.clone()).is_ok());
        for field in valid
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| key.as_str() != "id")
        {
            let mut broken = valid.clone();
            broken.as_object_mut().unwrap().remove(field);
            assert!(
                decode_discussion(broken).is_err(),
                "Accepted missing {field}"
            );
        }
        for (pointer, replacement) in [
            ("/number", json!(0)),
            ("/bodyText", Value::Null),
            ("/url", json!("bad URL")),
            ("/createdAt", json!("2026-02-30T00:00:00Z")),
            ("/updatedAt", json!("bad")),
            ("/closedAt", json!(false)),
            ("/answerChosenAt", json!("bad")),
            ("/closed", json!("true")),
            ("/isAnswered", json!("unknown")),
            ("/stateReason", json!(9)),
            ("/category/isAnswerable", json!("true")),
            ("/category/isAnswerable", Value::Null),
            ("/stateReason", json!("UNKNOWN")),
            ("/comments/totalCount", json!(-1)),
        ] {
            let mut broken = valid.clone();
            *broken.pointer_mut(pointer).unwrap() = replacement;
            assert!(decode_discussion(broken).is_err(), "Accepted {pointer}");
        }
        let mut broken = valid;
        broken["category"]
            .as_object_mut()
            .unwrap()
            .remove("isAnswerable");
        assert!(decode_discussion(broken).is_err());
    }
}
