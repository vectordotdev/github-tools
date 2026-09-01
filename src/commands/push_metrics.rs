use crate::commands::fetch_issues::parse_since;
use crate::config::Config;
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::blocking::Client;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_DD_SITE: &str = "datadoghq.com";
const DEFAULT_METRIC_PREFIX: &str = "github.health";
const DEFAULT_LOOKBACK: &str = "30d";
const DEFAULT_HISTORY: &str = "30d";
const DAY_SECONDS: i64 = 86_400;
const MAX_HISTORY_DAYS: i64 = 460;
const MAX_BATCH_SERIES: usize = 500;
const MAX_BATCH_BYTES: usize = 450_000;

#[derive(Debug, Clone, PartialEq, Serialize)]
struct MetricPoint {
    timestamp: i64,
    value: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct MetricSeries {
    metric: String,
    #[serde(rename = "type")]
    metric_type: u8,
    points: Vec<MetricPoint>,
    tags: Vec<String>,
}

impl MetricSeries {
    fn gauge(metric: String, value: i64, timestamp: i64, tags: Vec<String>) -> Self {
        Self {
            metric,
            metric_type: 3,
            points: vec![MetricPoint { timestamp, value }],
            tags,
        }
    }
}

#[derive(Clone, Copy)]
struct Snapshot<'a> {
    at: &'a str,
    window_start: &'a str,
    timestamp: i64,
}

pub fn run(
    config: &Config,
    dd_api_key: Option<&str>,
    dd_site: Option<&str>,
    history: Option<&str>,
    since: Option<&str>,
    prefix: Option<&str>,
    dry_run: bool,
    output_json: bool,
) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before the Unix epoch")?
        .as_secs() as i64;
    let history = history.unwrap_or(DEFAULT_HISTORY);
    let lookback = since.unwrap_or(DEFAULT_LOOKBACK);
    let history_since = parse_since(history)?;
    let history_start = DateTime::parse_from_rfc3339(&history_since)
        .with_context(|| format!("invalid resolved lookback timestamp: {history_since}"))?
        .timestamp();
    let snapshots = snapshot_timestamps(history_start, now)?;
    let activity_since = parse_since(lookback)?;
    let activity_start = DateTime::parse_from_rfc3339(&activity_since)
        .with_context(|| format!("invalid resolved activity timestamp: {activity_since}"))?
        .timestamp();
    let activity_seconds = now - activity_start;
    if activity_seconds <= 0 {
        anyhow::bail!("activity lookback must resolve to a time before now");
    }
    let metric_prefix = prefix.unwrap_or(DEFAULT_METRIC_PREFIX);
    validate_metric_prefix(metric_prefix)?;

    let db_path = format!("out/db/{}_{}.db", config.org, config.repo);
    let conn = Connection::open(&db_path)
        .with_context(|| format!("failed to open database: {db_path}"))?;

    println!("Reading metrics from {db_path}");
    println!(
        "  History: {history} ({} daily UTC snapshots plus the current point)",
        snapshots.len().saturating_sub(1)
    );
    println!("  Rolling activity window: {lookback}");

    let mut historical_series = Vec::new();
    for snapshot in snapshots {
        let snapshot_at = format_timestamp(snapshot)?;
        let window_start = format_timestamp(snapshot - activity_seconds)?;
        historical_series.extend(collect_metrics(
            &conn,
            config,
            metric_prefix,
            lookback,
            Snapshot {
                at: &snapshot_at,
                window_start: &window_start,
                timestamp: snapshot,
            },
        )?);
    }
    let series = coalesce_series(historical_series);

    if output_json {
        println!("{}", serde_json::to_string(&json_batch_output(&series)?)?);
        return Ok(());
    }

    if series.is_empty() {
        println!("No metrics to push.");
        return Ok(());
    }

    if dry_run {
        print_dry_run(&series);
        return Ok(());
    }

    let api_key = dd_api_key
        .filter(|key| !key.trim().is_empty())
        .context("DD_API_KEY not set (use --dd-api-key or the DD_API_KEY environment variable)")?;
    let api_url = datadog_api_url(dd_site.unwrap_or(DEFAULT_DD_SITE))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to construct Datadog HTTP client")?;

    let point_count = series
        .iter()
        .map(|metric| metric.points.len())
        .sum::<usize>();
    let batches = series_batches(&series)?;
    println!(
        "Pushing {} metric series with {point_count} historical/current points to Datadog...",
        series.len()
    );
    for (index, chunk) in batches.iter().enumerate() {
        let response = client
            .post(&api_url)
            .header("DD-API-KEY", api_key)
            .json(&serde_json::json!({ "series": chunk }))
            .send()
            .with_context(|| format!("failed to send Datadog batch {}", index + 1))?;

        let status = response.status();
        let body = response.text().unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Datadog rejected batch {} with {status}: {body}", index + 1);
        }
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body)
            && let Some(errors) = parsed.get("errors").and_then(|value| value.as_array())
            && !errors.is_empty()
        {
            anyhow::bail!(
                "Datadog returned errors for batch {}: {errors:?}",
                index + 1
            );
        }

        println!("  Batch {}: {} series accepted", index + 1, chunk.len());
    }

    println!("Done.");
    Ok(())
}

fn collect_metrics(
    conn: &Connection,
    config: &Config,
    prefix: &str,
    lookback: &str,
    snapshot: Snapshot<'_>,
) -> Result<Vec<MetricSeries>> {
    let base_tags = vec![
        format!(
            "repo:{}",
            sanitize_tag_value(&format!("{}/{}", config.org, config.repo))
        ),
        format!("owner:{}", sanitize_tag_value(&config.org)),
        format!("repository:{}", sanitize_tag_value(&config.repo)),
    ];

    let mut series = Vec::new();
    series.extend(open_items_gauge(
        conn, "issues", &base_tags, prefix, snapshot,
    )?);
    series.extend(open_items_gauge(
        conn,
        "pull_requests",
        &base_tags,
        prefix,
        snapshot,
    )?);
    series.extend(open_discussions_gauge(conn, &base_tags, prefix, snapshot)?);

    series.extend(closed_items_gauge(
        conn, "issues", &base_tags, lookback, prefix, snapshot,
    )?);
    series.extend(closed_items_gauge(
        conn,
        "pull_requests",
        &base_tags,
        lookback,
        prefix,
        snapshot,
    )?);
    series.extend(closed_discussions_gauge(
        conn, &base_tags, lookback, prefix, snapshot,
    )?);

    series.sort_by(|left, right| {
        left.metric
            .cmp(&right.metric)
            .then_with(|| left.tags.cmp(&right.tags))
    });
    Ok(series)
}

fn open_items_gauge(
    conn: &Connection,
    table: &str,
    base_tags: &[String],
    prefix: &str,
    snapshot: Snapshot<'_>,
) -> Result<Vec<MetricSeries>> {
    let metric_name = if table == "pull_requests" {
        format!("{prefix}.prs")
    } else {
        format!("{prefix}.issues")
    };
    let open_condition = "t.created_at <= ? AND (t.closed_at IS NULL OR t.closed_at > ?)";
    let label_map = build_label_map(conn, table, open_condition, &[snapshot.at, snapshot.at])?;
    let issue_type_col = if table == "issues" {
        ", issue_type"
    } else {
        ""
    };
    let extra_filter = if table == "pull_requests" {
        " AND is_draft = 0"
    } else {
        ""
    };
    let query = format!(
        "SELECT id, created_at{issue_type_col} FROM {table} \
         WHERE created_at <= ? AND (closed_at IS NULL OR closed_at > ?){extra_filter}"
    );
    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query(rusqlite::params![snapshot.at, snapshot.at])?;
    let mut counts: HashMap<Vec<String>, i64> = HashMap::new();

    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let created_at: String = row.get(1)?;
        let issue_type: Option<String> = if table == "issues" { row.get(2)? } else { None };
        let mut tags = base_tags.to_vec();
        tags.extend([
            "state:open".to_string(),
            age_bucket(&created_at, snapshot.timestamp),
        ]);
        add_issue_type_and_labels(&mut tags, issue_type.as_deref(), label_map.get(&id));
        tags.sort();
        *counts.entry(tags).or_default() += 1;
    }

    Ok(counts
        .into_iter()
        .map(|(tags, value)| {
            MetricSeries::gauge(metric_name.clone(), value, snapshot.timestamp, tags)
        })
        .collect())
}

fn closed_items_gauge(
    conn: &Connection,
    table: &str,
    base_tags: &[String],
    lookback: &str,
    prefix: &str,
    snapshot: Snapshot<'_>,
) -> Result<Vec<MetricSeries>> {
    let metric_name = if table == "pull_requests" {
        format!("{prefix}.prs.closed")
    } else {
        format!("{prefix}.issues.closed")
    };
    let label_map = build_label_map(
        conn,
        table,
        "t.closed_at >= ? AND t.closed_at <= ?",
        &[snapshot.window_start, snapshot.at],
    )?;
    let select_columns = if table == "issues" {
        "id, issue_type"
    } else {
        "id, NULL AS issue_type"
    };
    let extra_filter = if table == "pull_requests" {
        " AND is_draft = 0"
    } else {
        ""
    };
    let query = format!(
        "SELECT {select_columns} FROM {table} \
         WHERE closed_at >= ? AND closed_at <= ?{extra_filter}"
    );
    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query(rusqlite::params![snapshot.window_start, snapshot.at])?;
    let mut counts: HashMap<Vec<String>, i64> = HashMap::new();

    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let issue_type: Option<String> = row.get(1)?;
        let mut tags = base_tags.to_vec();
        tags.extend([
            "state:closed".to_string(),
            format!("window:{}", sanitize_tag_value(lookback)),
        ]);
        add_issue_type_and_labels(&mut tags, issue_type.as_deref(), label_map.get(&id));
        tags.sort();
        *counts.entry(tags).or_default() += 1;
    }

    Ok(counts
        .into_iter()
        .map(|(tags, value)| {
            MetricSeries::gauge(metric_name.clone(), value, snapshot.timestamp, tags)
        })
        .collect())
}

fn open_discussions_gauge(
    conn: &Connection,
    base_tags: &[String],
    prefix: &str,
    snapshot: Snapshot<'_>,
) -> Result<Vec<MetricSeries>> {
    let mut stmt = conn.prepare(
        "SELECT category, is_answered, created_at FROM discussions \
         WHERE created_at <= ? AND (closed_at IS NULL OR closed_at > ?)",
    )?;
    let mut rows = stmt.query(rusqlite::params![snapshot.at, snapshot.at])?;
    let mut counts: HashMap<Vec<String>, i64> = HashMap::new();

    while let Some(row) = rows.next()? {
        let category: String = row.get(0)?;
        let answered = row.get::<_, Option<bool>>(1)?.unwrap_or(false);
        let created_at: String = row.get(2)?;
        let mut tags = base_tags.to_vec();
        tags.extend([
            format!("category:{}", sanitize_tag_value(&category)),
            "state:open".to_string(),
            format!("answered:{answered}"),
            age_bucket(&created_at, snapshot.timestamp),
        ]);
        tags.sort();
        *counts.entry(tags).or_default() += 1;
    }

    Ok(counts
        .into_iter()
        .map(|(tags, value)| {
            MetricSeries::gauge(
                format!("{prefix}.discussions"),
                value,
                snapshot.timestamp,
                tags,
            )
        })
        .collect())
}

fn closed_discussions_gauge(
    conn: &Connection,
    base_tags: &[String],
    lookback: &str,
    prefix: &str,
    snapshot: Snapshot<'_>,
) -> Result<Vec<MetricSeries>> {
    let mut stmt = conn.prepare(
        "SELECT category, is_answered FROM discussions \
         WHERE closed_at >= ? AND closed_at <= ?",
    )?;
    let mut rows = stmt.query(rusqlite::params![snapshot.window_start, snapshot.at])?;
    let mut counts: HashMap<Vec<String>, i64> = HashMap::new();

    while let Some(row) = rows.next()? {
        let category: String = row.get(0)?;
        let answered = row.get::<_, Option<bool>>(1)?.unwrap_or(false);
        let mut tags = base_tags.to_vec();
        tags.extend([
            format!("category:{}", sanitize_tag_value(&category)),
            "state:closed".to_string(),
            format!("answered:{answered}"),
            format!("window:{}", sanitize_tag_value(lookback)),
        ]);
        tags.sort();
        *counts.entry(tags).or_default() += 1;
    }

    Ok(counts
        .into_iter()
        .map(|(tags, value)| {
            MetricSeries::gauge(
                format!("{prefix}.discussions.closed"),
                value,
                snapshot.timestamp,
                tags,
            )
        })
        .collect())
}

fn build_label_map(
    conn: &Connection,
    table: &str,
    condition: &str,
    parameters: &[&str],
) -> Result<HashMap<i64, Vec<String>>> {
    let query = format!(
        "SELECT il.issue_id, l.name \
         FROM issue_labels il \
         JOIN labels l ON l.id = il.label_id \
         JOIN {table} t ON t.id = il.issue_id \
         WHERE {condition}"
    );
    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(parameters.iter()))?;
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    while let Some(row) = rows.next()? {
        map.entry(row.get(0)?).or_default().push(row.get(1)?);
    }
    Ok(map)
}

fn add_issue_type_and_labels(
    tags: &mut Vec<String>,
    issue_type: Option<&str>,
    labels: Option<&Vec<String>>,
) {
    if let Some(issue_type) = issue_type {
        tags.push(format!("issue_type:{}", sanitize_tag_value(issue_type)));
    }
    if let Some(labels) = labels {
        tags.extend(labels.iter().map(|label| label_to_tag(label)));
    }
}

fn age_bucket(created_at: &str, now: i64) -> String {
    let age_days = chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|date| (now - date.timestamp()).max(0) / 86_400)
        .unwrap_or(0);
    let bucket = match age_days {
        0..=6 => "0_7d",
        7..=29 => "7_30d",
        30..=89 => "30_90d",
        _ => "90d_plus",
    };
    format!("age:{bucket}")
}

fn label_to_tag(label: &str) -> String {
    if let Some((key, value)) = label.split_once(':') {
        let key = sanitize_tag_key(key);
        if !key.is_empty() {
            return format!("{key}:{}", sanitize_tag_value(value));
        }
    }
    format!("label:{}", sanitize_tag_value(label))
}

fn sanitize_tag_key(value: &str) -> String {
    sanitize_tag_component(value, false)
}

fn sanitize_tag_value(value: &str) -> String {
    sanitize_tag_component(value, true)
}

fn sanitize_tag_component(value: &str, allow_slash: bool) -> String {
    let mut result = String::with_capacity(value.len());
    let mut previous_was_separator = false;
    for character in value.trim().to_lowercase().chars() {
        let allowed = character.is_ascii_alphanumeric()
            || matches!(character, '_' | '-' | '.')
            || (allow_slash && character == '/');
        if allowed {
            result.push(character);
            previous_was_separator = false;
        } else if !previous_was_separator && !result.is_empty() {
            result.push('_');
            previous_was_separator = true;
        }
        if result.len() >= 200 {
            break;
        }
    }
    result.trim_end_matches('_').to_string()
}

fn validate_metric_prefix(prefix: &str) -> Result<()> {
    if prefix.is_empty()
        || !prefix
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.'))
    {
        anyhow::bail!("metric prefix must contain only letters, numbers, underscores, and dots");
    }
    Ok(())
}

fn datadog_api_url(site: &str) -> Result<String> {
    let site = site.trim().trim_end_matches('/');
    if site.is_empty()
        || site.contains("://")
        || site.contains('/')
        || !site
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
    {
        anyhow::bail!("DD_SITE must be a hostname such as datadoghq.com or datadoghq.eu");
    }
    Ok(format!("https://api.{site}/api/v2/series"))
}

fn snapshot_timestamps(history_start: i64, now: i64) -> Result<Vec<i64>> {
    if history_start >= now {
        anyhow::bail!("lookback must resolve to a time before now");
    }
    if now - history_start > MAX_HISTORY_DAYS * DAY_SECONDS {
        anyhow::bail!("historical metric lookback cannot exceed {MAX_HISTORY_DAYS} days");
    }

    let mut snapshots = Vec::new();
    let mut timestamp = (history_start.div_euclid(DAY_SECONDS) + 1) * DAY_SECONDS;
    while timestamp < now {
        snapshots.push(timestamp);
        timestamp += DAY_SECONDS;
    }
    snapshots.push(now);
    Ok(snapshots)
}

fn format_timestamp(timestamp: i64) -> Result<String> {
    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
        .with_context(|| format!("invalid Unix timestamp: {timestamp}"))
}

fn coalesce_series(series: Vec<MetricSeries>) -> Vec<MetricSeries> {
    let mut grouped: BTreeMap<(String, u8, Vec<String>), Vec<MetricPoint>> = BTreeMap::new();
    for metric in series {
        grouped
            .entry((metric.metric, metric.metric_type, metric.tags))
            .or_default()
            .extend(metric.points);
    }

    grouped
        .into_iter()
        .map(|((metric, metric_type, tags), mut points)| {
            points.sort_by_key(|point| point.timestamp);
            points.dedup_by_key(|point| point.timestamp);
            MetricSeries {
                metric,
                metric_type,
                points,
                tags,
            }
        })
        .collect()
}

fn series_batches(series: &[MetricSeries]) -> Result<Vec<&[MetricSeries]>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut payload_bytes = 20;

    for (index, metric) in series.iter().enumerate() {
        let metric_bytes = serde_json::to_vec(metric)
            .context("failed to estimate Datadog metric payload size")?
            .len()
            + 1;
        if metric_bytes + 20 > MAX_BATCH_BYTES {
            anyhow::bail!("one Datadog metric series exceeds the safe payload size");
        }
        if index > start
            && (index - start >= MAX_BATCH_SERIES || payload_bytes + metric_bytes > MAX_BATCH_BYTES)
        {
            batches.push(&series[start..index]);
            start = index;
            payload_bytes = 20;
        }
        payload_bytes += metric_bytes;
    }

    if start < series.len() {
        batches.push(&series[start..]);
    }
    Ok(batches)
}

fn json_batch_output(series: &[MetricSeries]) -> Result<serde_json::Value> {
    let batches = series_batches(series)?
        .into_iter()
        .map(|batch| serde_json::json!({ "series": batch }))
        .collect::<Vec<_>>();
    let point_count = series
        .iter()
        .map(|metric| metric.points.len())
        .sum::<usize>();

    Ok(serde_json::json!({
        "format": "datadog-series-batches-v1",
        "series_count": series.len(),
        "point_count": point_count,
        "batches": batches,
    }))
}

fn print_dry_run(series: &[MetricSeries]) {
    let point_count = series
        .iter()
        .map(|metric| metric.points.len())
        .sum::<usize>();
    let total_items: i64 = series
        .iter()
        .flat_map(|metric| metric.points.iter())
        .map(|point| point.value)
        .sum();
    println!(
        "[dry-run] Would push {} unique series with {point_count} points representing {} grouped items across all snapshots",
        series.len(),
        total_items
    );

    let mut by_metric: HashMap<&str, (usize, usize, i64)> = HashMap::new();
    for metric in series {
        let entry = by_metric.entry(&metric.metric).or_default();
        entry.0 += 1;
        entry.1 += metric.points.len();
        entry.2 += metric.points.iter().map(|point| point.value).sum::<i64>();
    }
    let mut metrics: Vec<_> = by_metric.into_iter().collect();
    metrics.sort_by_key(|(name, _)| *name);
    for (name, (count, points, value)) in metrics {
        println!("  {name}: {count} series, {points} points, {value} grouped items");
    }

    println!("\n  Sample series:");
    for metric in series.iter().take(8) {
        let latest = metric.points.last().expect("metric series has a point");
        println!(
            "    {} = {} at {} ({} points): {}",
            metric.metric,
            latest.value,
            latest.timestamp,
            metric.points.len(),
            metric.tags.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE issues (
                id INTEGER PRIMARY KEY, state TEXT, created_at TEXT, closed_at TEXT, issue_type TEXT
            );
            CREATE TABLE pull_requests (
                id INTEGER PRIMARY KEY, state TEXT, created_at TEXT, closed_at TEXT,
                is_draft BOOLEAN
            );
            CREATE TABLE labels (id INTEGER PRIMARY KEY, name TEXT);
            CREATE TABLE issue_labels (issue_id INTEGER, label_id INTEGER);
            CREATE TABLE discussions (
                category TEXT, is_answered BOOLEAN, created_at TEXT, closed_at TEXT, closed BOOLEAN
            );

            INSERT INTO issues VALUES
                (1, 'open', '2026-08-29T00:00:00Z', NULL, 'Bug'),
                (2, 'closed', '2026-07-01T00:00:00Z', '2026-08-25T00:00:00Z', 'Feature');
            INSERT INTO pull_requests VALUES
                (3, 'open', '2026-08-01T00:00:00Z', NULL, 0),
                (4, 'closed', '2026-08-01T00:00:00Z', '2026-08-26T00:00:00Z', 0),
                (5, 'closed', '2026-08-01T00:00:00Z', '2026-08-27T00:00:00Z', 0),
                (6, 'open', '2026-08-01T00:00:00Z', NULL, 1);
            INSERT INTO labels VALUES (10, 'domain: API'), (11, 'good first issue');
            INSERT INTO issue_labels VALUES (1, 10), (2, 11), (4, 10);
            INSERT INTO discussions VALUES
                ('Q&A', 0, '2026-08-20T00:00:00Z', NULL, 0),
                ('Ideas', 1, '2026-08-01T00:00:00Z', '2026-08-24T00:00:00Z', 1);
            ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn creates_gauge_snapshots_for_open_and_windowed_activity() {
        let config = Config {
            github_token: String::new(),
            org: "Example-Org".to_string(),
            repo: "Example-Repo".to_string(),
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-31T00:00:00Z")
            .unwrap()
            .timestamp();
        let series = collect_metrics(
            &test_database(),
            &config,
            "test.github",
            "30d",
            Snapshot {
                at: "2026-08-31T00:00:00Z",
                window_start: "2026-08-01T00:00:00Z",
                timestamp: now,
            },
        )
        .unwrap();

        assert!(series.iter().all(|metric| metric.metric_type == 3));
        assert!(series.iter().all(|metric| {
            metric
                .tags
                .contains(&"repo:example-org/example-repo".to_string())
        }));
        assert!(series.iter().any(|metric| {
            metric.metric == "test.github.prs.closed"
                && metric.tags.contains(&"window:30d".to_string())
        }));
        assert!(
            !series
                .iter()
                .any(|metric| { metric.metric == "test.github.prs" && metric.points[0].value > 1 })
        );
    }

    #[test]
    fn reconstructs_backlog_at_historical_timestamps() {
        let config = Config {
            github_token: String::new(),
            org: "Example-Org".to_string(),
            repo: "Example-Repo".to_string(),
        };
        let historical = DateTime::parse_from_rfc3339("2026-08-23T00:00:00Z")
            .unwrap()
            .timestamp();
        let historical_series = collect_metrics(
            &test_database(),
            &config,
            "test.github",
            "30d",
            Snapshot {
                at: "2026-08-23T00:00:00Z",
                window_start: "2026-07-24T00:00:00Z",
                timestamp: historical,
            },
        )
        .unwrap();

        let issue_count = historical_series
            .iter()
            .filter(|metric| metric.metric == "test.github.issues")
            .map(|metric| metric.points[0].value)
            .sum::<i64>();
        let pr_count = historical_series
            .iter()
            .filter(|metric| metric.metric == "test.github.prs")
            .map(|metric| metric.points[0].value)
            .sum::<i64>();
        assert_eq!(issue_count, 1);
        assert_eq!(pr_count, 3);
    }

    #[test]
    fn creates_stable_daily_snapshots_and_coalesces_points() {
        let start = DateTime::parse_from_rfc3339("2026-08-28T12:00:00Z")
            .unwrap()
            .timestamp();
        let now = DateTime::parse_from_rfc3339("2026-08-31T16:00:00Z")
            .unwrap()
            .timestamp();
        let snapshots = snapshot_timestamps(start, now).unwrap();
        assert_eq!(
            snapshots
                .iter()
                .map(|timestamp| format_timestamp(*timestamp).unwrap())
                .collect::<Vec<_>>(),
            vec![
                "2026-08-29T00:00:00Z",
                "2026-08-30T00:00:00Z",
                "2026-08-31T00:00:00Z",
                "2026-08-31T16:00:00Z",
            ]
        );

        let tags = vec!["repo:example/repo".to_string()];
        let series = coalesce_series(vec![
            MetricSeries::gauge("test.metric".to_string(), 1, snapshots[0], tags.clone()),
            MetricSeries::gauge("test.metric".to_string(), 2, snapshots[1], tags),
        ]);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].points.len(), 2);
    }

    #[test]
    fn normalizes_label_tags_and_age_buckets() {
        assert_eq!(label_to_tag("domain: API Platform"), "domain:api_platform");
        assert_eq!(label_to_tag("Good First Issue"), "label:good_first_issue");
        assert_eq!(
            age_bucket("2026-08-25T00:00:00Z", 1_788_134_400),
            "age:0_7d"
        );
    }

    #[test]
    fn validates_datadog_site_and_metric_prefix() {
        assert_eq!(
            datadog_api_url("datadoghq.eu").unwrap(),
            "https://api.datadoghq.eu/api/v2/series"
        );
        assert!(datadog_api_url("https://example.com").is_err());
        assert!(validate_metric_prefix("github.health").is_ok());
        assert!(validate_metric_prefix("github-health").is_err());
    }

    #[test]
    fn emits_datadog_request_bodies_as_json_batches() {
        let series = vec![MetricSeries::gauge(
            "test.metric".to_string(),
            7,
            1_788_134_400,
            vec!["repo:example/repo".to_string()],
        )];

        let output = json_batch_output(&series).unwrap();
        assert_eq!(output["format"], "datadog-series-batches-v1");
        assert_eq!(output["series_count"], 1);
        assert_eq!(output["point_count"], 1);
        assert_eq!(output["batches"].as_array().unwrap().len(), 1);
        assert_eq!(output["batches"][0]["series"][0]["metric"], "test.metric");
        assert_eq!(output["batches"][0]["series"][0]["points"][0]["value"], 7);
    }
}
