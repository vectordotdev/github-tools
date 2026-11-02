//! Strict GraphQL requests and cursor pagination for repository snapshots.
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

const GRAPHQL_URL: &str = "https://api.github.com/graphql";
pub(super) const PAGE_SIZE: u32 = 100;

pub(super) fn request(
    client: &Client,
    config: &Config,
    query: &str,
    variables: Value,
) -> Result<Value> {
    request_at(client, config, GRAPHQL_URL, query, variables)
}

pub(super) fn request_at(
    client: &Client,
    config: &Config,
    endpoint: &str,
    query: &str,
    variables: Value,
) -> Result<Value> {
    let response = client
        .post(endpoint)
        .bearer_auth(&config.github_token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "github-tools")
        .timeout(Duration::from_secs(60))
        .json(&json!({"query": query, "variables": variables}))
        .send()
        .context("Failed to send GitHub GraphQL request")?;
    ensure!(
        response.status().is_success(),
        "GitHub GraphQL HTTP error: {}",
        response.status()
    );
    let mut value: Value = response
        .json()
        .context("Malformed GitHub GraphQL JSON response")?;
    // GitHub can return partial data alongside errors, including with HTTP 200.
    if let Some(errors) = value.get("errors") {
        bail!("GitHub GraphQL response contains errors: {errors}");
    }
    ensure!(
        value.get("data").is_some_and(Value::is_object),
        "Missing or invalid GraphQL data"
    );
    Ok(value["data"].take())
}

pub(super) fn connection(mut data: Value, path: &[&str]) -> Result<Value> {
    for field in path {
        data = data
            .get_mut(*field)
            .with_context(|| format!("Missing GraphQL connection {}", path.join(".")))?
            .take();
    }
    ensure!(
        data.is_object(),
        "Invalid GraphQL connection {}",
        path.join(".")
    );
    Ok(data)
}

pub(super) fn required<'a>(node: &'a Value, field: &str) -> Result<&'a Value> {
    node.get(field).with_context(|| format!("Missing {field}"))
}

pub(super) fn string<'a>(node: &'a Value, field: &str) -> Result<&'a str> {
    required(node, field)?
        .as_str()
        .with_context(|| format!("Invalid {field}: expected a string"))
}

pub(super) fn timestamp(node: &Value, field: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(string(node, field)?)
        .with_context(|| format!("Invalid {field}: expected an RFC 3339 timestamp"))
        .map(|date| date.with_timezone(&Utc))
}

pub(super) fn nullable_timestamp(node: &Value, field: &str) -> Result<()> {
    if !required(node, field)?.is_null() {
        timestamp(node, field)?;
    }
    Ok(())
}

pub(super) struct Page<'a> {
    pub nodes: &'a [Value],
    pub next_cursor: Option<&'a str>,
}

/// Validate the whole page, including nested pages that are already in a node.
pub(super) fn page(value: &Value) -> Result<Page<'_>> {
    let nodes = required(value, "nodes")?
        .as_array()
        .context("Invalid connection nodes")?;
    let info = required(value, "pageInfo")?;
    let has_next = required(info, "hasNextPage")?
        .as_bool()
        .context("Invalid hasNextPage")?;
    let cursor = match required(info, "endCursor")? {
        Value::Null => None,
        Value::String(cursor) => Some(cursor.as_str()),
        _ => bail!("Invalid endCursor: expected a string or null"),
    };
    let next_cursor = if has_next {
        ensure!(!nodes.is_empty(), "Empty page with hasNextPage=true");
        Some(
            cursor
                .filter(|cursor| !cursor.trim().is_empty())
                .context("Missing continuation cursor")?,
        )
    } else {
        None
    };
    let mut ids = HashSet::new();
    let mut numbers = HashSet::new();
    for node in nodes {
        let id = string(node, "id")?;
        ensure!(!id.trim().is_empty(), "Empty node ID");
        ensure!(ids.insert(id), "Duplicate node ID {id}");
        if let Some(number) = node.get("number") {
            let number = number
                .as_u64()
                .filter(|number| *number > 0)
                .context("Invalid record number")?;
            ensure!(numbers.insert(number), "Duplicate record number {number}");
        }
    }
    Ok(Page { nodes, next_cursor })
}

/// All six issue/PR/discussion modes, and nested labels, use this one loop.
/// A cutoff is valid only for connections explicitly ordered by UPDATED_AT DESC.
pub(super) fn paginate<T>(
    scope: &str,
    since: Option<&str>,
    mut fetch_page: impl FnMut(Option<&str>) -> Result<Value>,
    mut decode: impl FnMut(Value) -> Result<T>,
) -> Result<Vec<T>> {
    let cutoff = since
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .context("Invalid --since timestamp")
                .map(|date| date.with_timezone(&Utc))
        })
        .transpose()?;
    let mut after: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut ids = HashSet::new();
    let mut numbers = HashSet::new();
    let mut previous_updated = None;
    let mut records = Vec::new();
    let mut page_number = 1;
    loop {
        let context = format!("{scope}, page {page_number}");
        println!("Reading {context}...");
        let value = fetch_page(after.as_deref()).with_context(|| context.clone())?;
        let page = page(&value).with_context(|| context.clone())?;
        if let Some(cursor) = page.next_cursor {
            ensure!(
                cursors.insert(cursor.to_owned()),
                "{context}: repeated pagination cursor {cursor}"
            );
        }
        let mut hit_boundary = false;
        for node in page.nodes {
            let id = string(node, "id")?;
            ensure!(
                ids.insert(id.to_owned()),
                "{context}: duplicate node ID {id}"
            );
            if let Some(number) = node.get("number").and_then(Value::as_u64) {
                ensure!(
                    numbers.insert(number),
                    "{context}: duplicate record number {number}"
                );
            }
            let item_context = format!("{context}, node {id}");
            let updated = cutoff
                .as_ref()
                .map(|_| timestamp(node, "updatedAt"))
                .transpose()
                .with_context(|| item_context.clone())?;
            if let Some(updated) = updated {
                if let Some(previous) = previous_updated {
                    ensure!(
                        previous >= updated,
                        "{item_context}: UPDATED_AT order is not descending"
                    );
                }
                previous_updated = Some(updated);
            }
            // Validate even nodes outside the window before treating this page as a boundary.
            let record = decode(node.clone()).with_context(|| item_context.clone())?;
            if updated
                .zip(cutoff)
                .is_some_and(|(updated, cutoff)| updated < cutoff)
            {
                hit_boundary = true;
            } else {
                records.push(record);
            }
        }
        if hit_boundary || page.next_cursor.is_none() {
            break;
        }
        after = page.next_cursor.map(str::to_owned);
        page_number += 1;
    }
    Ok(records)
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    pub fn config() -> Config {
        Config {
            github_token: "fixture-credential".into(),
            org: "example".into(),
            repo: "project".into(),
        }
    }
    pub fn client() -> Client {
        Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }
    pub fn page(nodes: Vec<Value>, next: Option<&str>) -> Value {
        json!({"nodes": nodes, "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}})
    }
    pub fn ok(data: Value) -> (u16, String) {
        (200, json!({"data": data}).to_string())
    }

    pub struct TempDir(PathBuf);
    impl TempDir {
        pub fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "github-tools-fetch-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    pub struct Server {
        pub url: String,
        worker: JoinHandle<Vec<Value>>,
    }
    impl Server {
        pub fn new(responses: Vec<(u16, String)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/graphql", listener.local_addr().unwrap());
            let worker = thread::spawn(move || {
                let mut requests = Vec::new();
                for (status, body) in responses {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    Instant::now() < deadline,
                                    "Expected HTTP request was not made"
                                );
                                thread::sleep(Duration::from_millis(1));
                            }
                            Err(error) => panic!("{error}"),
                        }
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let header_end = loop {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).unwrap();
                        assert!(count > 0, "Request ended before headers");
                        bytes.extend_from_slice(&chunk[..count]);
                        if let Some(position) =
                            bytes.windows(4).position(|part| part == b"\r\n\r\n")
                        {
                            break position + 4;
                        }
                    };
                    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
                    assert!(headers.starts_with("POST /graphql HTTP/1.1"));
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    while bytes.len() < header_end + length {
                        let mut chunk = [0; 4096];
                        let count = stream.read(&mut chunk).unwrap();
                        assert!(count > 0, "Request ended before body");
                        bytes.extend_from_slice(&chunk[..count]);
                    }
                    requests.push(
                        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
                    );
                    let headers = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream
                        .write_all(format!("{headers}{body}").as_bytes())
                        .unwrap();
                }
                requests
            });
            Self { url, worker }
        }
        pub fn finish(self) -> Vec<Value> {
            self.worker.join().unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{Server, client, config, ok, page as fixture_page};
    use super::*;

    fn node(number: u64, updated: &str) -> Value {
        json!({"id": format!("N{number}"), "number": number, "updatedAt": updated})
    }
    fn collect(pages: Vec<Value>, since: Option<&str>) -> Result<Vec<Value>> {
        let mut pages = pages.into_iter();
        paginate(
            "fixture",
            since,
            |_| pages.next().context("Unexpected additional request"),
            Ok,
        )
    }

    #[test]
    fn follows_cursors_and_accepts_explicit_empty_completion() {
        let mut pages = vec![
            fixture_page(vec![node(1, "2026-03-01T00:00:00Z")], Some("a")),
            fixture_page(vec![node(2, "2026-03-01T00:00:00Z")], None),
        ]
        .into_iter();
        let mut afters = Vec::new();
        let records = paginate(
            "fixture",
            None,
            |after| {
                afters.push(after.map(str::to_owned));
                pages.next().context("Unexpected page")
            },
            Ok,
        )
        .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(afters, [None, Some("a".into())]);
        assert!(
            collect(vec![fixture_page(vec![], None)], None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_connections_fail_in_full_and_incremental_modes() {
        let good = fixture_page(vec![node(1, "2026-03-01T00:00:00Z")], None);
        let mut bad = vec![
            Value::Null,
            json!([]),
            json!({"nodes": [], "pageInfo": null}),
        ];
        for pointer in [
            "/nodes",
            "/pageInfo",
            "/pageInfo/hasNextPage",
            "/pageInfo/endCursor",
        ] {
            let mut value = good.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            value
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            bad.push(value);
        }
        for (pointer, replacement) in [
            ("/nodes", Value::Null),
            ("/nodes", json!([null])),
            ("/pageInfo/hasNextPage", Value::Null),
            ("/pageInfo/hasNextPage", json!("false")),
            ("/pageInfo/endCursor", json!(9)),
            ("/nodes/0/id", json!("")),
            ("/nodes/0/number", json!(0)),
            ("/nodes/0/number", json!("1")),
        ] {
            let mut value = good.clone();
            *value.pointer_mut(pointer).unwrap() = replacement;
            bad.push(value);
        }
        for cursor in [Value::Null, json!(""), json!(" ")] {
            bad.push(json!({"nodes": [node(1, "2026-03-01T00:00:00Z")], "pageInfo": {"hasNextPage": true, "endCursor": cursor}}));
        }
        bad.push(fixture_page(vec![], Some("a")));
        for since in [None, Some("2026-01-01T00:00:00Z")] {
            for value in &bad {
                assert!(
                    collect(vec![value.clone()], since).is_err(),
                    "Accepted malformed page: {value}"
                );
            }
        }
    }

    #[test]
    fn repeated_cursors_and_duplicate_identities_fail() {
        for cursors in [
            vec![Some("a"), Some("a")],
            vec![Some("a"), Some("b"), Some("a")],
        ] {
            let pages = cursors
                .into_iter()
                .enumerate()
                .map(|(i, cursor)| {
                    fixture_page(vec![node(i as u64 + 1, "2026-03-01T00:00:00Z")], cursor)
                })
                .collect();
            assert!(
                format!("{:#}", collect(pages, None).unwrap_err())
                    .contains("repeated pagination cursor")
            );
        }
        let first = node(1, "2026-03-01T00:00:00Z");
        for mut second in [first.clone(), node(2, "2026-03-01T00:00:00Z")] {
            second["number"] = json!(1);
            assert!(
                collect(
                    vec![
                        fixture_page(vec![first.clone()], Some("a")),
                        fixture_page(vec![second.clone()], None)
                    ],
                    None
                )
                .is_err()
            );
            assert!(collect(vec![fixture_page(vec![first.clone(), second], None)], None).is_err());
        }
    }

    #[test]
    fn cutoff_uses_instants_includes_equal_timestamps_and_stops_after_boundary() {
        let records = collect(
            vec![fixture_page(
                vec![
                    node(1, "2026-03-01T02:00:00+01:00"),
                    node(2, "2026-02-28T19:00:00-05:00"),
                    node(3, "2026-02-28T23:59:59Z"),
                ],
                Some("unused"),
            )],
            Some("2026-03-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            records
                .iter()
                .map(|node| node["number"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        let ties = collect(
            vec![
                fixture_page(vec![node(1, "2026-03-01T00:00:00Z")], Some("a")),
                fixture_page(vec![node(2, "2026-03-01T00:00:00Z")], None),
            ],
            Some("2026-03-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(ties.len(), 2);
    }

    #[test]
    fn malformed_dates_and_unsorted_boundaries_fail() {
        for pages in [
            vec![fixture_page(vec![node(1, "invalid")], None)],
            vec![fixture_page(
                vec![
                    node(1, "2025-12-01T00:00:00Z"),
                    node(2, "2026-03-01T00:00:00Z"),
                ],
                None,
            )],
            vec![
                fixture_page(vec![node(1, "2026-03-01T00:00:00Z")], Some("a")),
                fixture_page(vec![node(2, "2026-03-02T00:00:00Z")], None),
            ],
        ] {
            assert!(collect(pages, Some("2026-01-01T00:00:00Z")).is_err());
        }
        assert!(collect(vec![], Some("2026-02-30T00:00:00Z")).is_err());
        let result = paginate(
            "fixture",
            Some("2026-01-01T00:00:00Z"),
            |_| Ok(fixture_page(vec![node(1, "2025-12-01T00:00:00Z")], None)),
            |_| -> Result<Value> { bail!("Invalid old record") },
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("Invalid old record"));
    }

    #[test]
    fn transport_rejects_http_errors_partial_graphql_data_and_malformed_json() {
        let cfg = config();
        let client = client();
        let mut responses = vec![];
        for status in [301, 403, 429, 500, 503] {
            responses.push((status, json!({"data": {"repository": {}}}).to_string()));
        }
        responses.extend([
            (200, "{".into()),
            (200, json!({}).to_string()),
            (200, json!({"data": null}).to_string()),
            (200, json!({"data": []}).to_string()),
            (200, json!({"errors": [{"message": "limit"}]}).to_string()),
            (
                200,
                json!({"data": {"repository": {}}, "errors": [{"message": "partial"}]}).to_string(),
            ),
        ]);
        for response in responses {
            let server = Server::new(vec![response]);
            assert!(
                request_at(
                    &client,
                    &cfg,
                    &server.url,
                    "query Fixture { viewer { login } }",
                    json!({})
                )
                .is_err()
            );
            assert_eq!(server.finish().len(), 1);
        }
        let server = Server::new(vec![ok(
            json!({"repository": {"issues": fixture_page(vec![], None)}}),
        )]);
        let data = request_at(
            &client,
            &cfg,
            &server.url,
            "query Fixture { viewer { login } }",
            json!({}),
        )
        .unwrap();
        assert!(connection(data, &["repository", "issues"]).is_ok());
        server.finish();
        for data in [
            json!({}),
            json!({"repository": null}),
            json!({"repository": {"issues": null}}),
        ] {
            assert!(connection(data, &["repository", "issues"]).is_err());
        }
    }
}
