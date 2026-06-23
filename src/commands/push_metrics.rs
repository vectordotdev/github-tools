use crate::commands::fetch_issues::parse_since;
use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use rusqlite::Connection;
use serde_json::json;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const DD_API_URL: &str = "https://api.datadoghq.com/api/v2/series";
const BATCH_SIZE: usize = 500;

pub fn run(
    config: &Config,
    dd_api_key: &str,
    dd_site: Option<&str>,
    since: Option<&str>,
    prefix: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let metric_prefix = prefix.unwrap_or("cose.gh");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_secs() as i64;

    // `--since` controls the velocity window (items closed/merged in this period).
    // Open backlog is always all-time — no date filter.
    let velocity_since = parse_since(since.unwrap_or("30d"))?;

    let client = Client::new();
    let api_url = match dd_site {
        Some(site) => format!("https://api.{site}/api/v2/series"),
        None => DD_API_URL.to_string(),
    };

    let db_path = format!("out/db/{}_{}.db", config.org, config.repo);
    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open database: {db_path}"))?;

    let repo_tag = format!("repo:{}/{}", config.org, config.repo);

    println!("Reading from {db_path}...");
    println!("  Open backlog: all time");
    println!("  Velocity window: closed_at >= {velocity_since}");

    let mut all_series = Vec::new();

    // ── Open backlog gauges (all open items, no date filter, age bucket tag) ──
    all_series.extend(open_items_gauge(&conn, "issues", &repo_tag, metric_prefix, now)?);
    all_series.extend(open_items_gauge(&conn, "pull_requests", &repo_tag, metric_prefix, now)?);
    all_series.extend(open_discussions_gauge(&conn, &repo_tag, metric_prefix, now)?);

    // ── Velocity counts (items closed/merged since velocity_since) ──
    all_series.extend(closed_items_count(&conn, "issues", &repo_tag, &velocity_since, metric_prefix, now)?);
    all_series.extend(closed_items_count(&conn, "pull_requests", &repo_tag, &velocity_since, metric_prefix, now)?);

    if all_series.is_empty() {
        println!("No metrics to push.");
        return Ok(());
    }

    if dry_run {
        print_dry_run(&all_series);
        return Ok(());
    }

    println!("Pushing {} metric series to Datadog...", all_series.len());
    let mut failed_batches = 0usize;
    for (i, chunk) in all_series.chunks(BATCH_SIZE).enumerate() {
        let payload = json!({ "series": chunk });
        let response = client
            .post(&api_url)
            .header("DD-API-KEY", dd_api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .context("Failed to send metrics to Datadog")?;

        let status = response.status();
        let body = response.text().unwrap_or_default();
        if status.is_success() {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(errors) = parsed.get("errors").and_then(|e| e.as_array())
                && !errors.is_empty()
            {
                eprintln!("  Batch {}: {} errors:", i + 1, errors.len());
                for err in errors {
                    eprintln!("    {err}");
                }
                failed_batches += 1;
            } else {
                println!("  Batch {}: {} series", i + 1, chunk.len());
            }
        } else {
            eprintln!("  Batch {} rejected ({status}): {body}", i + 1);
            failed_batches += 1;
        }
    }

    if failed_batches > 0 {
        anyhow::bail!("{failed_batches} batch(es) failed; see errors above");
    }
    println!("Done.");
    Ok(())
}

/// Gauge: current count of open items grouped by tag combination, with age bucket tag.
/// No date filter — captures the full open backlog.
fn open_items_gauge(
    conn: &Connection,
    table: &str,
    repo_tag: &str,
    prefix: &str,
    now: i64,
) -> Result<Vec<serde_json::Value>> {
    let metric_name = if table == "pull_requests" {
        format!("{prefix}.prs")
    } else {
        format!("{prefix}.issues")
    };

    let label_map = build_label_map_open(conn, table)?;

    let issue_type_col = if table == "issues" { ", issue_type" } else { "" };
    let extra_filter = if table == "pull_requests" { " AND is_draft = 0" } else { "" };
    let query = format!(
        "SELECT id, created_at{issue_type_col} FROM {table} WHERE state = 'open'{extra_filter}"
    );

    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query([])?;

    let mut counts: HashMap<String, (Vec<String>, i64)> = HashMap::new();

    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let created_at: String = row.get(1)?;
        let issue_type: Option<String> = if table == "issues" { row.get(2)? } else { None };

        let mut tags = vec![
            repo_tag.to_string(),
            "state:open".to_string(),
            age_bucket(&created_at, now),
        ];

        if let Some(ref it) = issue_type {
            tags.push(format!("issue_type:{}", it.to_lowercase()));
        }

        if let Some(labels) = label_map.get(&id) {
            for label in labels {
                tags.push(label_to_tag(label));
            }
        }

        tags.sort();
        let key = tags.join(",");
        let entry = counts.entry(key).or_insert_with(|| (tags, 0));
        entry.1 += 1;
    }

    Ok(counts
        .into_values()
        .map(|(tags, count)| {
            json!({
                "metric": metric_name,
                "type": 3,  // gauge
                "points": [{"timestamp": now, "value": count}],
                "tags": tags,
            })
        })
        .collect())
}

/// Count: items closed/merged since `velocity_since`, grouped by tag combination.
/// Emitted at current timestamp — gives merge/close velocity for the period.
fn closed_items_count(
    conn: &Connection,
    table: &str,
    repo_tag: &str,
    velocity_since: &str,
    prefix: &str,
    now: i64,
) -> Result<Vec<serde_json::Value>> {
    let metric_name = if table == "pull_requests" {
        format!("{prefix}.prs.closed")
    } else {
        format!("{prefix}.issues.closed")
    };

    let label_map = build_label_map_closed_since(conn, table, velocity_since)?;

    let issue_type_col = if table == "issues" { ", issue_type" } else { "" };
    let extra_filter = if table == "pull_requests" { " AND is_draft = 0" } else { "" };
    let query = format!(
        "SELECT id{issue_type_col} FROM {table}
         WHERE state = 'closed' AND closed_at >= ?{extra_filter}"
    );

    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query(rusqlite::params![velocity_since])?;

    let mut counts: HashMap<String, (Vec<String>, i64)> = HashMap::new();

    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let issue_type: Option<String> = if table == "issues" { row.get(1)? } else { None };

        let mut tags = vec![repo_tag.to_string(), "state:closed".to_string()];

        if let Some(ref it) = issue_type {
            tags.push(format!("issue_type:{}", it.to_lowercase()));
        }

        if let Some(labels) = label_map.get(&id) {
            for label in labels {
                tags.push(label_to_tag(label));
            }
        }

        tags.sort();
        let key = tags.join(",");
        let entry = counts.entry(key).or_insert_with(|| (tags, 0));
        entry.1 += 1;
    }

    Ok(counts
        .into_values()
        .map(|(tags, count)| {
            json!({
                "metric": metric_name,
                "type": 1,  // count — number of close events in the velocity window
                "points": [{"timestamp": now, "value": count}],
                "tags": tags,
            })
        })
        .collect())
}

fn open_discussions_gauge(
    conn: &Connection,
    repo_tag: &str,
    prefix: &str,
    now: i64,
) -> Result<Vec<serde_json::Value>> {
    let query =
        "SELECT category, closed, is_answered, created_at FROM discussions WHERE closed = 0";

    let mut stmt = conn.prepare(query)?;
    let mut rows = stmt.query([])?;

    let mut counts: HashMap<String, (Vec<String>, i64)> = HashMap::new();

    while let Some(row) = rows.next()? {
        let category: String = row.get(0)?;
        let closed: bool = row.get(1)?;
        let is_answered: Option<bool> = row.get(2)?;
        let created_at: String = row.get(3)?;

        let answered = is_answered.unwrap_or(false);
        let state = if closed { "closed" } else if answered { "answered" } else { "open" };

        let mut tags = vec![
            repo_tag.to_string(),
            format!("category:{}", category.to_lowercase()),
            format!("state:{state}"),
            format!("answered:{answered}"),
            age_bucket(&created_at, now),
        ];
        tags.sort();

        let key = tags.join(",");
        let entry = counts.entry(key).or_insert_with(|| (tags, 0));
        entry.1 += 1;
    }

    Ok(counts
        .into_values()
        .map(|(tags, count)| {
            json!({
                "metric": format!("{prefix}.discussions"),
                "type": 3,  // gauge
                "points": [{"timestamp": now, "value": count}],
                "tags": tags,
            })
        })
        .collect())
}

fn build_label_map_open(conn: &Connection, table: &str) -> Result<HashMap<i64, Vec<String>>> {
    let query = format!(
        "SELECT il.issue_id, l.name
         FROM issue_labels il
         JOIN labels l ON l.id = il.label_id
         JOIN {table} t ON t.id = il.issue_id
         WHERE t.state = 'open'"
    );
    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query([])?;
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        map.entry(id).or_default().push(name);
    }
    Ok(map)
}

fn build_label_map_closed_since(
    conn: &Connection,
    table: &str,
    since: &str,
) -> Result<HashMap<i64, Vec<String>>> {
    let query = format!(
        "SELECT il.issue_id, l.name
         FROM issue_labels il
         JOIN labels l ON l.id = il.label_id
         JOIN {table} t ON t.id = il.issue_id
         WHERE t.state = 'closed' AND t.closed_at >= ?"
    );
    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query(rusqlite::params![since])?;
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        map.entry(id).or_default().push(name);
    }
    Ok(map)
}

fn age_bucket(created_at: &str, now: i64) -> String {
    let age_days = chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|dt| (now - dt.timestamp()).max(0) / 86400)
        .unwrap_or(0);
    let bucket = match age_days {
        0..=6 => "0-7d",
        7..=29 => "7-30d",
        30..=89 => "30-90d",
        _ => "90d+",
    };
    format!("age:{bucket}")
}

fn label_to_tag(label: &str) -> String {
    // "domain: vrl" -> "domain:vrl"
    // "good first issue" -> "label:good first issue"
    if let Some((prefix, value)) = label.split_once(": ") {
        format!("{prefix}:{value}")
    } else {
        format!("label:{label}")
    }
}

fn print_dry_run(all_series: &[serde_json::Value]) {
    let total_items: i64 = all_series
        .iter()
        .filter_map(|s| s["points"][0]["value"].as_i64())
        .sum();

    println!(
        "[dry-run] Would push {} series (representing {} total items) to Datadog",
        all_series.len(),
        total_items
    );

    let mut by_metric: HashMap<String, (usize, i64)> = HashMap::new();
    for s in all_series {
        let name = s["metric"].as_str().unwrap_or("unknown");
        let count = s["points"][0]["value"].as_i64().unwrap_or(0);
        let entry = by_metric.entry(name.to_string()).or_default();
        entry.0 += 1;
        entry.1 += count;
    }
    let mut metrics: Vec<_> = by_metric.into_iter().collect();
    metrics.sort();
    for (name, (series, count)) in &metrics {
        println!("  {name}: {series} series, {count} items");
    }

    println!("\n  Sample series:");
    for s in all_series.iter().take(8) {
        let metric = s["metric"].as_str().unwrap_or("");
        let count = s["points"][0]["value"].as_i64().unwrap_or(0);
        if let Some(tags) = s["tags"].as_array() {
            let tags_str: Vec<_> = tags.iter().filter_map(|t| t.as_str()).collect();
            println!("    {metric} = {count}: {}", tags_str.join(", "));
        }
    }
}
