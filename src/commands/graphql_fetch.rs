//! Strict GraphQL requests and cursor pagination for repository snapshots.
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

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

/// Use an explicit token or the existing authenticated GitHub CLI session.
pub(super) fn request_authenticated(
    client: &Client,
    config: &Config,
    query: &str,
    variables: Value,
) -> Result<Value> {
    request_checked(client, config, query, variables, false)
}

/// Account lookups may return NOT_FOUND for deleted users or bot identities.
/// All other GraphQL/transport failures remain fatal.
pub(super) fn request_accounts(client: &Client, config: &Config, query: &str) -> Result<Value> {
    request_checked(client, config, query, json!({}), true)
}

fn decode_response(value: Value, allow_missing_accounts: bool) -> Result<Value> {
    if let Some(errors) = value.get("errors") {
        ensure!(
            allow_missing_accounts,
            "GraphQL response contains errors: {errors}"
        );
        let errors = errors
            .as_array()
            .filter(|e| !e.is_empty())
            .context("Invalid GraphQL errors")?;
        for error in errors {
            let path = error["path"]
                .as_array()
                .context("Missing account error path")?;
            let alias = path
                .first()
                .and_then(Value::as_str)
                .context("Missing account alias")?;
            ensure!(
                error["type"] == "NOT_FOUND"
                    && path.len() == 1
                    && alias.starts_with('a')
                    && alias[1..].parse::<usize>().is_ok()
                    && value["data"].get(alias).is_some_and(Value::is_null),
                "GraphQL account lookup failed: {error}"
            );
        }
    }
    ensure!(value["data"].is_object(), "Missing GraphQL data");
    Ok(value["data"].clone())
}

fn request_checked(
    client: &Client,
    config: &Config,
    query: &str,
    variables: Value,
    allow_missing_accounts: bool,
) -> Result<Value> {
    if !config.github_token.is_empty() {
        if !allow_missing_accounts {
            return request(client, config, query, variables);
        }
        let response = client
            .post(GRAPHQL_URL)
            .bearer_auth(&config.github_token)
            .header("User-Agent", "github-tools")
            .timeout(Duration::from_secs(60))
            .json(&json!({"query":query,"variables":variables}))
            .send()?;
        ensure!(
            response.status().is_success(),
            "GitHub account lookup HTTP error: {}",
            response.status()
        );
        return decode_response(response.json()?, true);
    }
    // Reuse gh authentication without extracting its token.
    let executable = if cfg!(target_os = "macos") && Path::new("/opt/homebrew/bin/gh").exists() {
        "/opt/homebrew/bin/gh"
    } else {
        "gh"
    };
    request_gh(Command::new(executable), query, variables, allow_missing_accounts)
}

fn request_gh(
    mut command: Command,
    query: &str,
    variables: Value,
    allow_missing_accounts: bool,
) -> Result<Value> {
    let mut child = command
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
        output.status.success() || allow_missing_accounts && !output.stdout.is_empty(),
        "GitHub request failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        output.status.success() || value.get("errors").is_some(),
        "GitHub request failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    decode_response(value, allow_missing_accounts)
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
/// A cutoff filters records but never terminates pagination: GitHub does not
/// consistently order the exposed updatedAt values, even with that sort requested.
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
            // Validate every returned node, including records outside the window.
            let record = decode(node.clone()).with_context(|| item_context.clone())?;
            if updated
                .zip(cutoff)
                .is_some_and(|(updated, cutoff)| updated < cutoff)
            {
                continue;
            }
            records.push(record);
        }
        if page.next_cursor.is_none() {
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

    #[test]
    #[cfg(unix)]
    fn gh_transport_preserves_variables_and_rejects_partial_or_failed_responses() {
        let dir = super::test_support::TempDir::new();
        let path = dir.path().join("request.json");
        let fixture = |response: &str, code: u8| {
            let mut command = Command::new("/bin/sh");
            command.env_clear().args([
                "-c",
                "cat > \"$GH_FIXTURE_REQUEST\"; printf '%s' \"$GH_FIXTURE_RESPONSE\"; exit \"$GH_FIXTURE_EXIT\"",
            ]);
            command.env("GH_FIXTURE_REQUEST", &path)
                .env("GH_FIXTURE_RESPONSE", response)
                .env("GH_FIXTURE_EXIT", code.to_string());
            command
        };
        let query = "query($after: String) { viewer { login } }";
        let variables = json!({"after":"cursor-with-\"quotes\"\nand-newline"});
        assert_eq!(
            request_gh(fixture(r#"{"data":{"viewer":{"login":"example"}}}"#, 0), query, variables.clone(), false).unwrap(),
            json!({"viewer":{"login":"example"}})
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved, json!({"query":query,"variables":variables}));
        for (response, code) in [
            (r#"{"data":{"viewer":{"login":"example"}}}"#, 1),
            (r#"{"data":{"viewer":null},"errors":[{"type":"FORBIDDEN"}]}"#, 0),
            ("malformed-json", 0),
        ] {
            assert!(request_gh(fixture(response, code), query, json!({}), false).is_err());
        }
    }

    #[test]
    fn missing_account_lookups_are_unknown_but_partial_and_transport_errors_still_fail() {
        let missing = json!({"data":{"a0":null},"errors":[{"type":"NOT_FOUND","path":["a0"]}]});
        assert!(decode_response(missing.clone(), true).unwrap()["a0"].is_null());
        assert!(decode_response(missing, false).is_err());
        for error in [
            json!({"type":"RATE_LIMITED","path":["a0"]}),
            json!({"type":"NOT_FOUND","path":["repository"]}),
            json!({"type":"FORBIDDEN","path":["a0"]}),
        ] {
            assert!(decode_response(json!({"data":{"a0":null},"errors":[error]}), true).is_err());
        }
    }

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
    fn cutoff_uses_instants_and_includes_equal_timestamps() {
        let records = collect(
            vec![
                fixture_page(
                    vec![
                        node(1, "2026-03-01T02:00:00+01:00"),
                        node(2, "2026-02-28T19:00:00-05:00"),
                        node(3, "2026-02-28T23:59:59Z"),
                    ],
                    Some("next"),
                ),
                fixture_page(vec![], None),
            ],
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
    fn incremental_scan_does_not_stop_before_later_recent_records() {
        let records = collect(
            vec![
                fixture_page(vec![node(1, "2025-12-01T00:00:00Z")], Some("a")),
                fixture_page(vec![node(2, "2026-03-01T00:00:00Z")], Some("b")),
                fixture_page(
                    vec![
                        node(3, "2026-01-01T00:00:00Z"),
                        node(4, "2025-12-01T00:00:00Z"),
                    ],
                    None,
                ),
            ],
            Some("2026-01-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            records
                .iter()
                .map(|node| node["number"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [2, 3]
        );
        let records = collect(
            vec![fixture_page(
                vec![
                    node(1, "2025-12-01T00:00:00Z"),
                    node(2, "2026-03-01T00:00:00Z"),
                ],
                None,
            )],
            Some("2026-01-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["number"], 2);
    }

    #[test]
    fn malformed_dates_and_invalid_out_of_window_records_fail() {
        assert!(
            collect(
                vec![fixture_page(vec![node(1, "invalid")], None)],
                Some("2026-01-01T00:00:00Z")
            )
            .is_err()
        );
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
