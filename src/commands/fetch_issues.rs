use super::{graphql_fetch, sorted_json};
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Months, Utc};
use graphql_fetch::{PAGE_SIZE, nullable_timestamp, required, string, timestamp};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

const ISSUES_QUERY: &str = r#"
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    issues(first: $first, after: $after, orderBy: {field: CREATED_AT, direction: DESC}) {
      pageInfo {
        endCursor
        hasNextPage
      }
      nodes {
        id
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
          pageInfo { hasNextPage endCursor }
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
        id
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
          pageInfo { hasNextPage endCursor }
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
        id
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
          pageInfo { hasNextPage endCursor }
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
    let out_dir = Path::new("data")
        .join(format!("{}_{}", config.org, config.repo))
        .join("issues");
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
    if let Some(since) = &since_ts {
        DateTime::parse_from_rfc3339(since).context("Invalid --since timestamp")?;
    }
    let mut all_items = Vec::new();
    let mut node_ids = HashSet::new();
    for (field, is_pr) in [("issues", false), ("pullRequests", true)] {
        let query = if is_pr {
            PRS_QUERY
        } else if since_ts.is_some() {
            ISSUES_SINCE_QUERY
        } else {
            ISSUES_QUERY
        };
        let scope = format!("{}/{} {field}", config.org, config.repo);
        let items = graphql_fetch::paginate(
            &scope,
            if is_pr { since_ts.as_deref() } else { None },
            |after| {
                let mut variables = json!({"owner": config.org, "name": config.repo, "first": PAGE_SIZE, "after": after});
                if !is_pr && let Some(since) = &since_ts {
                    variables["since"] = json!(since);
                }
                graphql_fetch::connection(request(query, variables)?, &["repository", field])
            },
            |node| {
                let rest = to_rest_format(&node, is_pr)?;
                Ok((node, rest))
            },
        )?;
        println!("Total {field} fetched: {}", items.len());
        for (node, mut rest) in items {
            ensure!(
                node_ids.insert(string(&node, "id")?.to_owned()),
                "Issue and PR node IDs overlap in fetched repository"
            );
            if graphql_fetch::page(&node["labels"])?.next_cursor.is_some() {
                let mut first_page = Some(node["labels"].clone());
                let scope = format!("{scope} #{} labels", rest["number"]);
                let labels = graphql_fetch::paginate(
                    &scope,
                    None,
                    |after| {
                        if let Some(first) = first_page.take() {
                            return Ok(first);
                        }
                        graphql_fetch::connection(
                            request(
                                LABELS_QUERY,
                                json!({"id": node["id"], "first": PAGE_SIZE, "after": after}),
                            )?,
                            &["node", "labels"],
                        )
                    },
                    |label| label_to_rest(&label),
                )?;
                rest["labels"] = json!(labels);
            }
            all_items.push(rest);
        }
    }
    let mut numbers = HashSet::new();
    for item in &all_items {
        let number = required(item, "number")?
            .as_u64()
            .context("Invalid converted record number")?;
        ensure!(
            numbers.insert(number),
            "Issue and PR numbers overlap in fetched repository"
        );
    }
    // No directories or snapshots are touched until both collections and all nested pages succeed.
    fs::create_dir_all(out_dir)?;
    let written = write_year_bucketed(&all_items, out_dir, "created_at")?;
    for (year, count) in &written {
        println!("  {year}.json: wrote {count} items");
    }
    Ok(())
}

const LABELS_QUERY: &str = r#"
query($id: ID!, $first: Int!, $after: String) {
  node(id: $id) {
    ... on Labelable {
      labels(first: $first, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes { id name color description }
      }
    }
  }
}
"#;

/// Parse a --since value: ISO date (2026-01-01), YYYY-MM (2026-01), or relative (3m, 1y, 30d).
pub fn parse_since(input: &str) -> Result<String> {
    parse_since_at(input, Utc::now())
}

fn parse_since_at(input: &str, now: DateTime<Utc>) -> Result<String> {
    // Relative: e.g. "3m", "1y", "30d"
    if let Some(num_str) = input.strip_suffix('d') {
        let days: i64 = num_str.parse().context("Invalid number in relative date")?;
        let dt = now - chrono::Duration::days(days);
        return Ok(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }
    if let Some(num_str) = input.strip_suffix('m') {
        let months: u32 = num_str.parse().context("Invalid number in relative date")?;
        let dt = now
            .checked_sub_months(Months::new(months))
            .context("Relative month value is out of range")?;
        return Ok(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());
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
fn write_year_bucketed(
    items: &[Value],
    out_dir: &Path,
    date_field: &str,
) -> Result<BTreeMap<String, usize>> {
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
            existing
                .into_iter()
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

        let json_str = sorted_json(&merged)?;
        fs::write(&path, json_str)?;
        counts.insert(year.clone(), new_items.len());
    }
    Ok(counts)
}

/// Convert only complete, validated API records to the existing snapshot shape.
fn to_rest_format(node: &Value, is_pr: bool) -> Result<Value> {
    let id = required(node, "databaseId")?
        .as_i64()
        .filter(|id| *id > 0)
        .context("Invalid databaseId")?;
    let number = required(node, "number")?
        .as_u64()
        .filter(|number| *number > 0)
        .context("Invalid number")?;
    let title = string(node, "title")?;
    let state = match string(node, "state")? {
        "OPEN" => "open",
        "CLOSED" => "closed",
        "MERGED" if is_pr => "closed",
        state => bail!(
            "Invalid {} state {state}",
            if is_pr { "PR" } else { "issue" }
        ),
    };
    timestamp(node, "createdAt")?;
    timestamp(node, "updatedAt")?;
    nullable_timestamp(node, "closedAt")?;
    let author = required(node, "author")?;
    let login = if author.is_null() {
        None
    } else {
        Some(string(author, "login")?)
    };
    let issue_type = if is_pr {
        None
    } else {
        let value = required(node, "issueType")?;
        if value.is_null() {
            None
        } else {
            Some(string(value, "name")?)
        }
    };
    let labels = required(node, "labels")?;
    let labels = graphql_fetch::page(labels)?
        .nodes
        .iter()
        .map(label_to_rest)
        .collect::<Result<Vec<_>>>()?;
    let mut item = json!({
        "id": id, "number": number, "title": title, "state": state,
        "created_at": node["createdAt"], "updated_at": node["updatedAt"], "closed_at": node["closedAt"],
        "user": {"login": login}, "labels": labels, "issue_type": issue_type,
    });
    if is_pr {
        let draft = required(node, "isDraft")?
            .as_bool()
            .context("Invalid isDraft")?;
        item["pull_request"] = json!({});
        item["draft"] = json!(draft);
    }
    Ok(item)
}

fn label_to_rest(label: &Value) -> Result<Value> {
    let id = string(label, "id")?;
    ensure!(!id.trim().is_empty(), "Empty label ID");
    let name = string(label, "name")?;
    let color = string(label, "color")?;
    ensure!(
        color.len() == 6 && color.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Invalid label color"
    );
    let description = required(label, "description")?;
    ensure!(
        description.is_null() || description.is_string(),
        "Invalid label description"
    );
    Ok(json!({"id": stable_id(id), "name": name, "color": color, "description": description}))
}

/// Produce a stable i64 from a GraphQL node ID string, for use as a database key.
fn stable_id(node_id: &str) -> i64 {
    let mut hasher = DefaultHasher::new();
    node_id.hash(&mut hasher);
    // Mask to positive i64
    (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as i64
}

#[cfg(test)]
mod tests {
    use super::parse_since_at;
    use chrono::{TimeZone, Utc};

    #[test]
    fn incremental_save_normalizes_retained_records_and_is_repeatable() {
        use serde_json::{Value, json};
        use std::fs;

        let dir = std::env::temp_dir().join(format!(
            "github-tools-sorted-snapshot-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026.json");
        let retained = json!({"title": "retained", "number": 1, "created_at": "2026-01-01"});
        let original = json!([retained, {"title": "old", "number": 2, "obsolete": true}]);
        fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();
        let fresh = json!({"title": "new", "number": 2, "created_at": "2026-01-02", "labels": [{"name": "bug", "id": 3}]});
        let added = json!({"title": "added", "number": 3, "created_at": "2026-01-03"});
        let inputs = [added.clone(), fresh.clone()];

        super::write_year_bucketed(&inputs, &dir, "created_at").unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        let expected = json!([retained, fresh, added]);
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), expected);
        assert_eq!(saved, super::sorted_json(&expected).unwrap());
        super::write_year_bucketed(&inputs, &dir, "created_at").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn relative_months_preserve_day_and_time() {
        let now = Utc.with_ymd_and_hms(2026, 9, 3, 14, 25, 30).unwrap();

        assert_eq!(parse_since_at("3m", now).unwrap(), "2026-06-03T14:25:30Z");
    }
}

#[cfg(test)]
mod fetch_safety_tests {
    use super::*;
    use crate::commands::graphql_fetch::test_support::{Server, TempDir, client, config, ok, page};

    fn label(number: u64) -> Value {
        json!({"id": format!("L{number}"), "name": format!("label {number}"), "color": "abcdef", "description": null})
    }
    fn node(number: u64, is_pr: bool) -> Value {
        let mut value = json!({
            "id": format!("{}{number}", if is_pr { "PR" } else { "I" }), "databaseId": 100 + number,
            "number": number, "title": format!("Item {number}"), "state": "OPEN",
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-03-01T00:00:00Z", "closedAt": null,
            "author": null, "labels": page(vec![label(1)], None),
        });
        if is_pr {
            value["isDraft"] = json!(false);
        } else {
            value["issueType"] = Value::Null;
        }
        value
    }
    fn data(field: &str, nodes: Vec<Value>, next: Option<&str>) -> Value {
        let mut data = json!({"repository": {}});
        data["repository"][field] = page(nodes, next);
        data
    }
    fn failures(phase: &str) -> Vec<((u16, String), &'static str)> {
        let malformed = if phase.ends_with("labels") {
            let mut invalid = label(2);
            invalid["color"] = json!("invalid");
            json!({"node": {"labels": page(vec![invalid], None)}})
        } else {
            let mut invalid = node(3, phase == "pullRequests");
            invalid["title"] = Value::Null;
            data(phase, vec![invalid], None)
        };
        vec![
            (ok(malformed), "Invalid"),
            ((503, json!({"data": {}}).to_string()), "503"),
            (
                (
                    200,
                    json!({"data": {"repository": {}}, "errors": [{"message": "fixture failure"}]})
                        .to_string(),
                ),
                "fixture failure",
            ),
            ((200, "{".into()), "Malformed GitHub GraphQL JSON"),
        ]
    }

    #[test]
    fn later_page_failures_preserve_all_year_files_in_every_issue_and_pr_mode() {
        for since in [None, Some("2026-01-01")] {
            for phase in ["issues", "pullRequests", "issue-labels", "pr-labels"] {
                for (failure, marker) in failures(phase) {
                    let dir = TempDir::new();
                    let before = [
                        ("2025.json", b"[{\"number\":80,\"retained\":true}]".to_vec()),
                        ("2026.json", b"[{\"number\":90,\"retained\":true}]".to_vec()),
                    ];
                    for (name, bytes) in &before {
                        fs::write(dir.path().join(name), bytes).unwrap();
                    }
                    let mut issue = node(1, false);
                    let mut pr = node(2, true);
                    let responses = match phase {
                        "issues" => vec![
                            ok(data("issues", vec![issue], Some("next-issues"))),
                            failure,
                        ],
                        "pullRequests" => vec![
                            ok(data("issues", vec![issue], None)),
                            ok(data("pullRequests", vec![pr], Some("next-prs"))),
                            failure,
                        ],
                        "issue-labels" => {
                            issue["labels"] = page(vec![label(1)], Some("next-labels"));
                            vec![ok(data("issues", vec![issue], None)), failure]
                        }
                        "pr-labels" => {
                            pr["labels"] = page(vec![label(1)], Some("next-labels"));
                            vec![
                                ok(data("issues", vec![], None)),
                                ok(data("pullRequests", vec![pr], None)),
                                failure,
                            ]
                        }
                        _ => unreachable!(),
                    };
                    let count = responses.len();
                    let server = Server::new(responses);
                    let cfg = config();
                    let client = client();
                    let error = run_with_request(&cfg, since, dir.path(), |query, variables| {
                        graphql_fetch::request_at(&client, &cfg, &server.url, query, variables)
                    })
                    .unwrap_err();
                    let message = format!("{error:#}");
                    assert!(message.contains(marker), "{phase}: {message}");
                    assert!(message.contains("page 2"), "{phase}: {message}");
                    for (name, bytes) in before {
                        assert_eq!(fs::read(dir.path().join(name)).unwrap(), bytes);
                    }
                    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
                    let requests = server.finish();
                    assert_eq!(requests.len(), count);
                    assert_eq!(
                        requests.last().unwrap()["variables"]["after"],
                        if phase.ends_with("labels") {
                            "next-labels"
                        } else if phase == "issues" {
                            "next-issues"
                        } else {
                            "next-prs"
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn healthy_full_and_incremental_modes_preserve_snapshot_shape_and_follow_each_cursor() {
        for since in [None, Some("2026-01-01")] {
            let dir = TempDir::new();
            let server = Server::new(vec![
                ok(data("issues", vec![node(1, false)], Some("issues-next"))),
                ok(data("issues", vec![node(2, false)], None)),
                ok(data("pullRequests", vec![node(3, true)], Some("prs-next"))),
                ok(data("pullRequests", vec![node(4, true)], None)),
            ]);
            let cfg = config();
            let client = client();
            run_with_request(&cfg, since, dir.path(), |query, variables| {
                graphql_fetch::request_at(&client, &cfg, &server.url, query, variables)
            })
            .unwrap();
            let saved: Vec<Value> =
                serde_json::from_slice(&fs::read(dir.path().join("2026.json")).unwrap()).unwrap();
            assert_eq!(saved.len(), 4);
            assert_eq!(
                saved
                    .iter()
                    .map(|row| row["number"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                [1, 2, 3, 4]
            );
            for (index, row) in saved.iter().enumerate() {
                assert_eq!(row["id"], 101 + index);
                assert_eq!(row["state"], "open");
                assert_eq!(row["created_at"], "2026-01-01T00:00:00Z");
                assert_eq!(row["updated_at"], "2026-03-01T00:00:00Z");
                assert!(row["closed_at"].is_null());
                assert_eq!(row["user"], json!({"login": null}));
                assert_eq!(
                    row["labels"],
                    json!([{"id": stable_id("L1"), "name": "label 1", "color": "abcdef", "description": null}])
                );
                assert_eq!(row.get("pull_request").is_some(), index >= 2);
                assert_eq!(
                    row.get("draft"),
                    if index >= 2 {
                        Some(&Value::Bool(false))
                    } else {
                        None
                    }
                );
            }
            let requests = server.finish();
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request["variables"]["after"].clone())
                    .collect::<Vec<_>>(),
                [
                    Value::Null,
                    json!("issues-next"),
                    Value::Null,
                    json!("prs-next")
                ]
            );
            assert_eq!(
                requests[0]["variables"].get("since").is_some(),
                since.is_some()
            );
            for (index, request) in requests.into_iter().enumerate() {
                assert_eq!(request["variables"]["first"], PAGE_SIZE);
                assert_eq!(
                    request["query"]
                        .as_str()
                        .unwrap()
                        .contains("field: UPDATED_AT"),
                    since.is_some() && index < 2
                );
            }
        }
    }

    #[test]
    fn incremental_issues_keep_all_server_filtered_matches() {
        let dir = TempDir::new();
        let mut item = node(1, false);
        item["createdAt"] = json!("2025-01-01T00:00:00Z");
        item["updatedAt"] = json!("2025-12-31T00:00:00Z");
        let server = Server::new(vec![
            ok(data("issues", vec![item], None)),
            ok(data("pullRequests", vec![], None)),
        ]);
        let cfg = config();
        let client = client();
        run_with_request(&cfg, Some("2026-01-01"), dir.path(), |query, variables| {
            graphql_fetch::request_at(&client, &cfg, &server.url, query, variables)
        })
        .unwrap();
        let saved: Vec<Value> =
            serde_json::from_slice(&fs::read(dir.path().join("2025.json")).unwrap()).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["updated_at"], "2025-12-31T00:00:00Z");
        let requests = server.finish();
        assert_eq!(requests[0]["variables"]["since"], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn nested_label_pagination_keeps_more_than_the_first_hundred_labels() {
        for is_pr in [false, true] {
            let dir = TempDir::new();
            let mut item = node(1, is_pr);
            item["labels"] = page((1..=100).map(label).collect(), Some("label-100"));
            let field = if is_pr { "pullRequests" } else { "issues" };
            let mut responses = vec![];
            if is_pr {
                responses.push(ok(data("issues", vec![], None)));
            }
            responses.push(ok(data(field, vec![item], None)));
            responses.push(ok(
                json!({"node": {"labels": page(vec![label(101)], None)}}),
            ));
            if !is_pr {
                responses.push(ok(data("pullRequests", vec![], None)));
            }
            let server = Server::new(responses);
            let cfg = config();
            let client = client();
            run_with_request(&cfg, None, dir.path(), |query, variables| {
                graphql_fetch::request_at(&client, &cfg, &server.url, query, variables)
            })
            .unwrap();
            let saved: Vec<Value> =
                serde_json::from_slice(&fs::read(dir.path().join("2026.json")).unwrap()).unwrap();
            assert_eq!(saved[0]["labels"].as_array().unwrap().len(), 101);
            assert_eq!(saved[0]["labels"][100]["name"], "label 101");
            let requests = server.finish();
            let nested = &requests[if is_pr { 2 } else { 1 }];
            assert!(
                nested["query"]
                    .as_str()
                    .unwrap()
                    .contains("... on Labelable")
            );
            assert_eq!(nested["variables"]["id"], if is_pr { "PR1" } else { "I1" });
            assert_eq!(nested["variables"]["after"], "label-100");
        }
    }

    #[test]
    fn invalid_api_records_do_not_become_default_values() {
        for is_pr in [false, true] {
            let valid = node(1, is_pr);
            let mut fields = vec![
                "databaseId",
                "number",
                "title",
                "state",
                "createdAt",
                "updatedAt",
                "closedAt",
                "author",
                "labels",
            ];
            fields.push(if is_pr { "isDraft" } else { "issueType" });
            for field in fields {
                let mut broken = valid.clone();
                broken.as_object_mut().unwrap().remove(field);
                assert!(
                    to_rest_format(&broken, is_pr).is_err(),
                    "Accepted missing {field}"
                );
            }
            for (pointer, replacement) in [
                ("/databaseId", Value::Null),
                ("/databaseId", json!(0)),
                ("/number", json!(-1)),
                ("/title", json!(1)),
                ("/state", json!("UNKNOWN")),
                ("/createdAt", json!("2026-02-30T00:00:00Z")),
                ("/updatedAt", json!("bad")),
                ("/closedAt", json!("bad")),
                ("/author", json!({})),
                ("/labels/nodes/0/id", json!("")),
                ("/labels/nodes/0/color", json!("zzzzzz")),
                ("/labels/nodes/0/description", json!(9)),
                ("/labels/pageInfo/hasNextPage", Value::Null),
            ] {
                let mut broken = valid.clone();
                *broken.pointer_mut(pointer).unwrap() = replacement;
                assert!(
                    to_rest_format(&broken, is_pr).is_err(),
                    "Accepted {pointer}"
                );
            }
            let mut broken = valid.clone();
            broken[if is_pr { "isDraft" } else { "issueType" }] =
                if is_pr { Value::Null } else { json!({}) };
            assert!(to_rest_format(&broken, is_pr).is_err());
        }
        let mut issue = node(1, false);
        issue["state"] = json!("CLOSED");
        assert!(to_rest_format(&issue, false).unwrap()["closed_at"].is_null());
        issue["state"] = json!("MERGED");
        assert!(to_rest_format(&issue, false).is_err());
        let mut pr = node(1, true);
        pr["state"] = json!("MERGED");
        assert_eq!(to_rest_format(&pr, true).unwrap()["state"], "closed");
        pr["isDraft"] = json!(true);
        assert_eq!(to_rest_format(&pr, true).unwrap()["draft"], true);
    }
}
