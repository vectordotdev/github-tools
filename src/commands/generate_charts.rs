use anyhow::{Context, Result};
use charming::{
    Chart,
    component::{Axis, Grid, Legend, VisualMap, VisualMapType},
    element::{
        AxisLabel, AxisType, Color, ItemStyle, LineStyle, LineStyleType, Tooltip, Trigger,
    },
    series::{Bar, Heatmap, Line},
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

// ── Color palette ──────────────────────────────────────────────────────────────

const COLOR_CLOSED: &str = "#006400";
const COLOR_OPEN: &str = "#FF8C00";
const COLOR_ANSWERED: &str = "#4C9AFF";
const COLOR_CREATED: &str = "#1a1a1a"; // DARK in Python — near-black for all "created" series
const COLOR_NEW_CONTRIBUTOR: &str = "#36B37E";
const COLOR_RETURNING_CONTRIBUTOR: &str = "#8E5CE6";

const PALETTE: &[&str] = &[
    "#4C9AFF", "#36B37E", "#FF8C00", "#FF4C4C", "#6554C0",
    "#00B8D9", "#57D9A3", "#FFC400", "#FF7452", "#998DD9",
    "#79E2F2", "#ABF5D1", "#FFE380", "#FFBDAD", "#B3D4FF",
];

const EXCLUDE_LABELS: &[&str] = &["no-changelog", "meta: awaiting author"];

const KNOWN_BOT_LOGINS: &[&str] = &[
    "dependabot", "dependabot-preview", "renovate",
    "handlerbot", "step-security-bot", "tronboto",
];

fn is_bot(login: &str) -> bool {
    login.ends_with("[bot]") || KNOWN_BOT_LOGINS.contains(&login)
}

fn label_color(name: &str) -> &'static str {
    match name {
        "type: bug"  | "Bug"                => "#FF4C4C",
        "type: feature" | "Feature"         => COLOR_ANSWERED,
        "type: enhancement" | "Enhancement" => COLOR_NEW_CONTRIBUTOR,
        "type: task" | "Task"               => "#FFA500",
        "domain: external docs"             => "#afab7e",
        "domain: ci"                        => "#d6c720",
        "dependencies" | "domain: deps"     => "#1f3f18",
        "domain: core"                      => "#b50036",
        "domain: sources"                   => "#2dbcbc",
        "domain: transforms"                => "#8615bf",
        "domain: sinks"                     => "#ad4f47",
        _ => {
            // DJB2 hash
            let hash = name.bytes().fold(5381u64, |h, b| {
                h.wrapping_mul(33).wrapping_add(b as u64)
            });
            PALETTE[(hash as usize) % PALETTE.len()]
        }
    }
}

fn color(s: &str) -> Color {
    Color::Value(s.to_string())
}

// ── CSV reading helpers ─────────────────────────────────────────────────────────

/// Read a CSV into Vec<HashMap<String, String>>. Returns empty if file missing.
fn read_csv(path: &str) -> Result<Vec<HashMap<String, String>>> {
    if !Path::new(path).exists() {
        return Ok(vec![]);
    }
    let mut rdr = csv::Reader::from_path(path)
        .with_context(|| format!("opening {path}"))?;
    let headers: Vec<String> = rdr.headers()?.iter().map(|s| s.to_string()).collect();
    let mut rows = Vec::new();
    for result in rdr.records() {
        let record = result?;
        let mut map = HashMap::new();
        for (i, val) in record.iter().enumerate() {
            if let Some(h) = headers.get(i) {
                map.insert(h.clone(), val.to_string());
            }
        }
        rows.push(map);
    }
    Ok(rows)
}

/// Filter rows by start date (YYYY-MM prefix comparison on the "month" column).
fn filter_by_start<'a>(rows: &'a [HashMap<String, String>], start: Option<&str>) -> Vec<&'a HashMap<String, String>> {
    match start {
        None => rows.iter().collect(),
        Some(s) => rows.iter().filter(|r| {
            r.get("month").map(|m| m.as_str() >= s).unwrap_or(false)
        }).collect(),
    }
}

fn parse_i64(s: &str) -> i64 {
    s.trim().parse().unwrap_or(0)
}

// ── Chart builders ─────────────────────────────────────────────────────────────

/// 1. Monthly trend: line chart with created vs closed series.
///    Also optionally adds dashed overlay lines for label type columns.
///
/// `type_overlays`: (series_name, columns_to_sum) — mirrors Python's df[matching].sum(axis=1).
pub fn monthly_trend(
    rows: &[&HashMap<String, String>],
    created_col: &str,
    closed_col: &str,
    type_overlays: &[(String, Vec<String>)],
) -> Chart {
    let months: Vec<String> = rows.iter()
        .filter_map(|r| r.get("month").cloned())
        .collect();
    let created: Vec<i64> = rows.iter()
        .map(|r| parse_i64(r.get(created_col).map(|s| s.as_str()).unwrap_or("0")))
        .collect();
    let closed: Vec<i64> = rows.iter()
        .map(|r| parse_i64(r.get(closed_col).map(|s| s.as_str()).unwrap_or("0")))
        .collect();

    let mut chart = Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .legend(Legend::new().bottom("0"))
        .grid(Grid::new().left("3%").right("4%").bottom("15%").contain_label(true))
        .x_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(months.clone())
                .axis_label(AxisLabel::new().rotate(45.0).interval(2.0)),
        )
        .y_axis(Axis::new().type_(AxisType::Value))
        .series(
            Line::new()
                .name("Created")
                .data(created.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_CREATED))),
        )
        .series(
            Line::new()
                .name("Closed")
                .data(closed.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_CLOSED))),
        );

    for (series_name, cols) in type_overlays {
        let vals: Vec<i64> = rows.iter()
            .map(|r| cols.iter().map(|c| parse_i64(r.get(c.as_str()).map(|s| s.as_str()).unwrap_or("0"))).sum())
            .collect();
        let c = label_color(series_name);
        chart = chart.series(
            Line::new()
                .name(series_name.as_str())
                .data(vals.to_vec())
                .line_style(LineStyle::new().type_(LineStyleType::Dashed))
                .item_style(ItemStyle::new().color(color(c))),
        );
    }

    chart
}

/// 2. Discussion trend: 3-series line chart.
pub fn discussion_trend(rows: &[&HashMap<String, String>]) -> Chart {
    let months: Vec<String> = rows.iter()
        .filter_map(|r| r.get("month").cloned())
        .collect();
    let created: Vec<i64> = rows.iter()
        .map(|r| parse_i64(r.get("created_discussions").map(|s| s.as_str()).unwrap_or("0")))
        .collect();
    let closed: Vec<i64> = rows.iter()
        .map(|r| parse_i64(r.get("closed_discussions").map(|s| s.as_str()).unwrap_or("0")))
        .collect();
    let answered: Vec<i64> = rows.iter()
        .map(|r| parse_i64(r.get("answered_discussions").map(|s| s.as_str()).unwrap_or("0")))
        .collect();

    Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .legend(Legend::new().bottom("0"))
        .grid(Grid::new().left("3%").right("4%").bottom("15%").contain_label(true))
        .x_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(months)
                .axis_label(AxisLabel::new().rotate(45.0).interval(2.0)),
        )
        .y_axis(Axis::new().type_(AxisType::Value))
        .series(
            Line::new()
                .name("Created")
                .data(created.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_CREATED))),
        )
        .series(
            Line::new()
                .name("Closed")
                .data(closed.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_CLOSED))),
        )
        .series(
            Line::new()
                .name("Answered")
                .data(answered.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_ANSWERED))),
        )
}

/// 3. Label breakdown: horizontal bar chart, top 20 labels.
pub fn label_breakdown(rows: &[&HashMap<String, String>]) -> Chart {
    let mut pairs: Vec<(String, i64)> = rows.iter()
        .filter_map(|r| {
            let name = r.get("label_name")?.clone();
            if EXCLUDE_LABELS.contains(&name.as_str()) { return None; }
            let count = parse_i64(r.get("count").map(|s| s.as_str()).unwrap_or("0"));
            Some((name, count))
        })
        .collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1));
    pairs.truncate(20);

    let labels: Vec<String> = pairs.iter().map(|(n, _)| n.clone()).collect();

    // Build bar data with per-item color
    let bar_data: Vec<charming::datatype::DataPointItem> = pairs.iter()
        .map(|(name, count)| {
            let col = label_color(name);
            charming::datatype::DataPointItem::new(*count)
                .item_style(ItemStyle::new().color(color(col)))
        })
        .collect();

    Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .grid(Grid::new().left("5%").right("4%").top("5%").bottom("5%").contain_label(true))
        .x_axis(Axis::new().type_(AxisType::Value))
        .y_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(labels)
                .inverse(true),
        )
        .series(Bar::new().name("Count").data(bar_data))
}

/// 4. Label counts over time: grouped vertical bar chart, top 8 labels.
pub fn label_counts_over_time(rows: &[&HashMap<String, String>]) -> Chart {
    // Pivot: month -> label -> count
    let mut pivot: BTreeMap<String, HashMap<String, i64>> = BTreeMap::new();
    let mut label_totals: HashMap<String, i64> = HashMap::new();

    for row in rows {
        let month = match row.get("month") { Some(m) => m.clone(), None => continue };
        let label = match row.get("label_name") { Some(l) => l.clone(), None => continue };
        if EXCLUDE_LABELS.contains(&label.as_str()) { continue; }
        let count = parse_i64(row.get("count").map(|s| s.as_str()).unwrap_or("0"));
        *pivot.entry(month.clone()).or_default().entry(label.clone()).or_insert(0) += count;
        *label_totals.entry(label).or_insert(0) += count;
    }

    let mut top_labels: Vec<(String, i64)> = label_totals.into_iter().collect();
    top_labels.sort_by(|a, b| b.1.cmp(&a.1));
    top_labels.truncate(8);
    let top_labels: Vec<String> = top_labels.into_iter().map(|(l, _)| l).collect();

    let months: Vec<String> = pivot.keys().cloned().collect();

    let mut chart = Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .legend(Legend::new().bottom("0"))
        .grid(Grid::new().left("3%").right("4%").bottom("20%").contain_label(true))
        .x_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(months.clone())
                .axis_label(AxisLabel::new().rotate(45.0).interval(2.0)),
        )
        .y_axis(Axis::new().type_(AxisType::Value));

    for label in &top_labels {
        let vals: Vec<i64> = months.iter()
            .map(|m| *pivot.get(m).and_then(|lm| lm.get(label.as_str())).unwrap_or(&0))
            .collect();
        let c = label_color(label);
        chart = chart.series(
            Bar::new()
                .name(label.as_str())
                .data(vals.to_vec())
                .item_style(ItemStyle::new().color(color(c))),
        );
    }

    chart
}

/// 5. Open/closed by integration label: stacked horizontal bar chart.
///
/// Filters to source:/transform:/sink: labels only, sorted by total descending.
pub fn open_closed_by_label(rows: &[&HashMap<String, String>]) -> Option<Chart> {
    let mut pairs: Vec<(String, i64, i64)> = rows.iter()
        .filter_map(|r| {
            let name = r.get("label_name")?.clone();
            if EXCLUDE_LABELS.contains(&name.as_str()) { return None; }
            if !name.starts_with("source:") && !name.starts_with("transform:") && !name.starts_with("sink:") {
                return None;
            }
            let open = parse_i64(r.get("open_count").map(|s| s.as_str()).unwrap_or("0"));
            let closed = parse_i64(r.get("closed_count").map(|s| s.as_str()).unwrap_or("0"));
            Some((name, open, closed))
        })
        .collect();
    if pairs.is_empty() { return None; }
    // Sort by total (open + closed) descending, top 30
    pairs.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)));
    pairs.truncate(30);

    let labels: Vec<String> = pairs.iter().map(|(n, _, _)| n.clone()).collect();
    let closed_vals: Vec<i64> = pairs.iter().map(|(_, _, c)| *c).collect();
    let open_vals: Vec<i64> = pairs.iter().map(|(_, o, _)| *o).collect();

    Some(Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .legend(Legend::new().bottom("0"))
        .grid(Grid::new().left("5%").right("4%").top("5%").bottom("15%").contain_label(true))
        .x_axis(Axis::new().type_(AxisType::Value))
        .y_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(labels)
                .inverse(true),
        )
        .series(
            Bar::new()
                .name("Closed")
                .stack("total")
                .data(closed_vals.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_CLOSED))),
        )
        .series(
            Bar::new()
                .name("Open")
                .stack("total")
                .data(open_vals.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_OPEN))),
        ))
}

/// 6. Integration trends: multi-line chart for source:/transform:/sink: columns.
pub fn integration_trends(
    rows: &[&HashMap<String, String>],
    all_columns: &[String],
) -> Option<Chart> {
    // Find integration columns
    let integration_cols: Vec<&String> = all_columns.iter()
        .filter(|c| {
            c.starts_with("source:") || c.starts_with("transform:") || c.starts_with("sink:")
        })
        .collect();

    if integration_cols.is_empty() {
        return None;
    }

    // Pick top 5 by total count
    let mut totals: Vec<(&String, i64)> = integration_cols.iter()
        .map(|col| {
            let total: i64 = rows.iter()
                .map(|r| parse_i64(r.get(col.as_str()).map(|s| s.as_str()).unwrap_or("0")))
                .sum();
            (*col, total)
        })
        .collect();
    totals.sort_by(|a, b| b.1.cmp(&a.1));
    totals.truncate(5);

    let top_cols: Vec<&String> = totals.into_iter().map(|(c, _)| c).collect();
    if top_cols.is_empty() {
        return None;
    }

    let months: Vec<String> = rows.iter()
        .filter_map(|r| r.get("month").cloned())
        .collect();

    let mut chart = Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .legend(Legend::new().bottom("0"))
        .grid(Grid::new().left("3%").right("4%").bottom("20%").contain_label(true))
        .x_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(months.clone())
                .axis_label(AxisLabel::new().rotate(45.0).interval(2.0)),
        )
        .y_axis(Axis::new().type_(AxisType::Value));

    for (i, col) in top_cols.iter().enumerate() {
        let vals: Vec<i64> = rows.iter()
            .map(|r| parse_i64(r.get(col.as_str()).map(|s| s.as_str()).unwrap_or("0")))
            .collect();
        let c = PALETTE[i % PALETTE.len()];
        chart = chart.series(
            Line::new()
                .name(col.as_str())
                .data(vals.to_vec())
                .item_style(ItemStyle::new().color(color(c))),
        );
    }

    Some(chart)
}

/// 7. Contributor heatmap: months x top 10 contributors.
pub fn contributor_heatmap(rows: &[&HashMap<String, String>]) -> Option<Chart> {
    if rows.is_empty() { return None; }

    // Collect all months, sorted, take last 12
    let mut all_months: Vec<String> = rows.iter()
        .filter_map(|r| r.get("month").cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    all_months.sort();
    if all_months.len() > 12 {
        all_months = all_months[all_months.len() - 12..].to_vec();
    }

    let month_set: HashSet<&str> = all_months.iter().map(|s| s.as_str()).collect();

    // Top 10 contributors by total PRs
    let mut user_totals: HashMap<String, i64> = HashMap::new();
    for row in rows {
        let month = match row.get("month") { Some(m) => m, None => continue };
        if !month_set.contains(month.as_str()) { continue; }
        let user = match row.get("user_login") { Some(u) => u.clone(), None => continue };
        let count = parse_i64(row.get("count").map(|s| s.as_str()).unwrap_or("0"));
        *user_totals.entry(user).or_insert(0) += count;
    }
    let mut top_users: Vec<(String, i64)> = user_totals.into_iter().collect();
    top_users.sort_by(|a, b| b.1.cmp(&a.1));
    top_users.truncate(10);
    let top_users: Vec<String> = top_users.into_iter().map(|(u, _)| u).collect();

    if top_users.is_empty() { return None; }

    // Build heatmap data: [[month_idx, user_idx, count], ...]
    // Each row is Vec<DataPoint> (a DataFrame row)
    let month_idx: HashMap<&str, usize> = all_months.iter().enumerate().map(|(i, m)| (m.as_str(), i)).collect();
    let user_idx: HashMap<&str, usize> = top_users.iter().enumerate().map(|(i, u)| (u.as_str(), i)).collect();

    use charming::datatype::DataPoint;
    let mut heat_data: Vec<Vec<DataPoint>> = Vec::new();
    for row in rows {
        let month = match row.get("month") { Some(m) => m, None => continue };
        let user = match row.get("user_login") { Some(u) => u, None => continue };
        let mi = match month_idx.get(month.as_str()) { Some(i) => *i, None => continue };
        let ui = match user_idx.get(user.as_str()) { Some(i) => *i, None => continue };
        let count = parse_i64(row.get("count").map(|s| s.as_str()).unwrap_or("0"));
        heat_data.push(vec![
            DataPoint::from(mi as i64),
            DataPoint::from(ui as i64),
            DataPoint::from(count),
        ]);
    }

    let max_val: i64 = {
        let mut max = 1i64;
        for row in rows {
            let month = match row.get("month") { Some(m) => m, None => continue };
            if !month_set.contains(month.as_str()) { continue; }
            let count = parse_i64(row.get("count").map(|s| s.as_str()).unwrap_or("0"));
            if count > max { max = count; }
        }
        max
    };

    Some(Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Item))
        .grid(Grid::new().left("5%").right("4%").bottom("20%").contain_label(true))
        .x_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(all_months.clone())
                .axis_label(AxisLabel::new().rotate(45.0).interval(0.0)),
        )
        .y_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(top_users.clone())
                .inverse(true),
        )
        .visual_map(
            VisualMap::new()
                .type_(VisualMapType::Continuous)
                .min(0.0)
                .max(max_val as f64)
                .calculable(true)
                .orient(charming::element::Orient::Horizontal)
                .left("center")
                .bottom("5%"),
        )
        .series(
            Heatmap::new()
                .name("PRs")
                .data(heat_data),
        ))
}

/// Classify new vs returning contributors.
fn classify_contributors(rows: &[&HashMap<String, String>]) -> BTreeMap<String, (i64, i64)> {
    // Returns BTreeMap<month, (new_count, returning_count)>
    let mut months: Vec<String> = rows.iter()
        .filter_map(|r| r.get("month").cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    months.sort();

    let mut seen: HashSet<String> = HashSet::new();
    let mut result: BTreeMap<String, (i64, i64)> = BTreeMap::new();

    // Group by month
    let mut by_month: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        let month = match row.get("month") { Some(m) => m.clone(), None => continue };
        let user = match row.get("user_login") { Some(u) => u.clone(), None => continue };
        by_month.entry(month).or_default().push(user);
    }

    for month in &months {
        let users = by_month.get(month).cloned().unwrap_or_default();
        let mut new_count = 0i64;
        let mut ret_count = 0i64;
        for user in &users {
            if seen.contains(user) {
                ret_count += 1;
            } else {
                new_count += 1;
                seen.insert(user.clone());
            }
        }
        result.insert(month.clone(), (new_count, ret_count));
    }
    result
}

/// 8. Unique contributors monthly: stacked bar with new vs returning.
pub fn unique_contributors_monthly(rows: &[&HashMap<String, String>]) -> Option<Chart> {
    if rows.is_empty() { return None; }

    let classified = classify_contributors(rows);
    if classified.is_empty() { return None; }

    let months: Vec<String> = classified.keys().cloned().collect();
    let new_vals: Vec<i64> = classified.values().map(|(n, _)| *n).collect();
    let ret_vals: Vec<i64> = classified.values().map(|(_, r)| *r).collect();

    Some(Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .legend(Legend::new().bottom("0"))
        .grid(Grid::new().left("3%").right("4%").bottom("15%").contain_label(true))
        .x_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(months)
                .axis_label(AxisLabel::new().rotate(45.0).interval(2.0)),
        )
        .y_axis(Axis::new().type_(AxisType::Value))
        .series(
            Bar::new()
                .name("New")
                .stack("contributors")
                .data(new_vals.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_NEW_CONTRIBUTOR))),
        )
        .series(
            Bar::new()
                .name("Returning")
                .stack("contributors")
                .data(ret_vals.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_RETURNING_CONTRIBUTOR))),
        ))
}

/// 9. Unique contributors yearly: same logic but aggregated by year.
pub fn unique_contributors_yearly(rows: &[&HashMap<String, String>]) -> Option<Chart> {
    if rows.is_empty() { return None; }

    // Aggregate monthly contributor data into yearly
    let mut by_year: BTreeMap<String, Vec<HashMap<String, String>>> = BTreeMap::new();
    for row in rows {
        let month = match row.get("month") { Some(m) => m.clone(), None => continue };
        let year = month[..4.min(month.len())].to_string();
        let mut synthetic = HashMap::new();
        synthetic.insert("month".to_string(), year.clone());
        if let Some(u) = row.get("user_login") { synthetic.insert("user_login".to_string(), u.clone()); }
        if let Some(c) = row.get("count") { synthetic.insert("count".to_string(), c.clone()); }
        by_year.entry(year).or_default().push(synthetic);
    }

    // Classify using yearly aggregated rows
    let all_rows_owned: Vec<HashMap<String, String>> = by_year.into_values().flatten().collect();
    let refs: Vec<&HashMap<String, String>> = all_rows_owned.iter().collect();
    let classified = classify_contributors(&refs);
    if classified.is_empty() { return None; }

    let years: Vec<String> = classified.keys().cloned().collect();
    let new_vals: Vec<i64> = classified.values().map(|(n, _)| *n).collect();
    let ret_vals: Vec<i64> = classified.values().map(|(_, r)| *r).collect();

    Some(Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .legend(Legend::new().bottom("0"))
        .grid(Grid::new().left("3%").right("4%").bottom("15%").contain_label(true))
        .x_axis(
            Axis::new()
                .type_(AxisType::Category)
                .data(years),
        )
        .y_axis(Axis::new().type_(AxisType::Value))
        .series(
            Bar::new()
                .name("New")
                .stack("contributors")
                .data(new_vals.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_NEW_CONTRIBUTOR))),
        )
        .series(
            Bar::new()
                .name("Returning")
                .stack("contributors")
                .data(ret_vals.to_vec())
                .item_style(ItemStyle::new().color(color(COLOR_RETURNING_CONTRIBUTOR))),
        ))
}

// ── AI review stats ──────────────────────────────────────────────────────────────

struct AiStats {
    prs_scanned: u64,
    bot_login: String,
    since: Option<String>,
    total: u64,
    liked: u64,
    disliked: u64,
    no_signal: u64,
}

fn read_ai_stats(input_dir: &str, prefix: &str) -> Option<AiStats> {
    let path = Path::new(input_dir).join(format!("{prefix}_automated_review_stats.json"));
    let text = fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(AiStats {
        prs_scanned: v["prs_scanned"].as_u64()?,
        bot_login:   v["bot_login"].as_str()?.to_string(),
        since:       v["since"].as_str().map(|s| s.to_string()),
        total:       v["total"].as_u64()?,
        liked:       v["liked"].as_u64()?,
        disliked:    v["disliked"].as_u64()?,
        no_signal:   v["no_signal"].as_u64()?,
    })
}

fn ai_stats_entry(stats: &AiStats) -> Result<ChartEntry> {
    let chart = Chart::new()
        .tooltip(Tooltip::new().trigger(Trigger::Axis))
        .grid(Grid::new().left("5%").right("4%").top("5%").bottom("5%").contain_label(true))
        .x_axis(Axis::new().type_(AxisType::Value))
        .y_axis(Axis::new().type_(AxisType::Category).data(vec!["No Signal", "Disliked 👎", "Liked 👍"]).inverse(true))
        .series(
            Bar::new().name("Comments").data(vec![
                charming::datatype::DataPointItem::new(stats.no_signal as i64)
                    .item_style(ItemStyle::new().color(color("#adb5bd"))),
                charming::datatype::DataPointItem::new(stats.disliked as i64)
                    .item_style(ItemStyle::new().color(color("#FF4C4C"))),
                charming::datatype::DataPointItem::new(stats.liked as i64)
                    .item_style(ItemStyle::new().color(color(COLOR_NEW_CONTRIBUTOR))),
            ])
        );

    let reacted = stats.liked + stats.disliked;
    let since_str = stats.since.as_deref().unwrap_or("all time");
    let meta = format!(
        "{} merged PRs &nbsp;·&nbsp; bot: <code>{}</code> &nbsp;·&nbsp; since: {}",
        stats.prs_scanned, html_escape(&stats.bot_login), since_str
    );

    let all_table = format!(
        "<p class=\"stats-label\">All comments</p>\
        <table class=\"stats-table\">\
        <thead><tr><th>Reaction</th><th>Count</th><th>Share</th></tr></thead>\
        <tbody>\
        <tr><td>Liked 👍</td><td>{liked}</td><td>{liked_pct:.1}%</td></tr>\
        <tr><td>Disliked 👎</td><td>{disliked}</td><td>{disliked_pct:.1}%</td></tr>\
        <tr><td>No signal</td><td>{no_signal}</td><td>{no_signal_pct:.1}%</td></tr>\
        <tr><td><strong>Total</strong></td><td><strong>{total}</strong></td><td></td></tr>\
        </tbody></table>",
        liked = stats.liked, disliked = stats.disliked,
        no_signal = stats.no_signal, total = stats.total,
        liked_pct = stats.liked as f64 / stats.total as f64 * 100.0,
        disliked_pct = stats.disliked as f64 / stats.total as f64 * 100.0,
        no_signal_pct = stats.no_signal as f64 / stats.total as f64 * 100.0,
    );

    let reacted_table = if reacted > 0 {
        format!(
            "<p class=\"stats-label\">Reacted comments only (excludes no signal)</p>\
            <table class=\"stats-table\">\
            <thead><tr><th>Reaction</th><th>Count</th><th>Share</th></tr></thead>\
            <tbody>\
            <tr><td>Liked 👍</td><td>{liked}</td><td>{liked_pct:.1}%</td></tr>\
            <tr><td>Disliked 👎</td><td>{disliked}</td><td>{disliked_pct:.1}%</td></tr>\
            <tr><td><strong>Total</strong></td><td><strong>{reacted}</strong></td><td></td></tr>\
            </tbody></table>",
            liked = stats.liked, disliked = stats.disliked, reacted = reacted,
            liked_pct = stats.liked as f64 / reacted as f64 * 100.0,
            disliked_pct = stats.disliked as f64 / reacted as f64 * 100.0,
        )
    } else {
        String::new()
    };

    let extra = format!("<p class=\"stats-meta\">{meta}</p>{all_table}{reacted_table}");
    let json = serde_json::to_string(&chart).context("serializing ai stats chart")?;
    Ok(ChartEntry {
        title: "AI Code Review".to_string(),
        note: None,
        json,
        height_px: 180,
        extra_html: Some(extra),
    })
}

// ── HTML generation ─────────────────────────────────────────────────────────────

struct ChartEntry {
    title: String,
    note: Option<String>,
    json: String,
    height_px: u32,
    extra_html: Option<String>,
}

fn chart_to_entry(title: &str, note: Option<&str>, chart: &Chart) -> Result<ChartEntry> {
    let json = serde_json::to_string(chart)
        .with_context(|| format!("serializing chart '{title}'"))?;
    Ok(ChartEntry { title: title.to_string(), note: note.map(|s| s.to_string()), json, height_px: 400, extra_html: None })
}

fn chart_to_entry_h(title: &str, note: Option<&str>, chart: &Chart, height_px: u32) -> Result<ChartEntry> {
    let json = serde_json::to_string(chart)
        .with_context(|| format!("serializing chart '{title}'"))?;
    Ok(ChartEntry { title: title.to_string(), note: note.map(|s| s.to_string()), json, height_px, extra_html: None })
}

fn read_data_notes(repo_name: &str) -> Option<String> {
    let path = format!("trends/{repo_name}.md");
    let text = fs::read_to_string(&path).ok()?;

    // Find the ## Data notes section
    let start_marker = "## Data notes";
    let start_pos = text.find(start_marker)?;
    let after_header = start_pos + start_marker.len();

    // Find the next ## heading after the section content
    let section_text = &text[after_header..];
    let end_pos = section_text.find("\n## ").map(|p| p + 1).unwrap_or(section_text.len());
    let content = &section_text[..end_pos];

    // Convert the markdown fragment to HTML
    let mut html = String::new();
    let mut in_list = false;

    for line in content.lines() {
        if line.trim_start().starts_with("- ") {
            if !in_list {
                html.push_str("<ul>");
                in_list = true;
            }
            let item = &line.trim_start()[2..];
            html.push_str(&format!("<li>{}</li>", inline_md_to_html(item)));
        } else {
            if in_list {
                html.push_str("</ul>");
                in_list = false;
            }
            // skip blank and non-list lines
        }
    }
    if in_list {
        html.push_str("</ul>");
    }

    if html.is_empty() {
        return None;
    }

    Some(format!("<div class=\"notes\"><h3>Data notes</h3>{html}</div>"))
}

fn update_yearly_contributors_md(
    repo_name: &str,
    yearly_stats: &BTreeMap<String, (i64, i64)>,
) -> Result<()> {
    use chrono::{Datelike, Utc};
    let trends_path = Path::new("trends").join(format!("{repo_name}.md"));
    if !trends_path.exists() { return Ok(()); }

    let current_year = Utc::now().year();
    let current_month = Utc::now().month(); // 1-based
    let completed_months = current_month - 1; // months with complete data

    let mut md_rows = String::from("\n");
    md_rows.push_str("| Year | Unique | New | Returning |\n");
    md_rows.push_str("|------|--------|-----|----------|\n");
    for (year, (new_c, ret_c)) in yearly_stats {
        let unique = new_c + ret_c;
        let year_label = if year.parse::<i32>().ok() == Some(current_year) {
            format!("{year} (YTD, {completed_months}mo)")
        } else {
            year.clone()
        };
        md_rows.push_str(&format!("| {year_label} | {unique} | {new_c} | {ret_c} |\n"));
    }

    const START: &str = "<!-- AUTO:yearly-contributors:start -->";
    const END: &str = "<!-- AUTO:yearly-contributors:end -->";
    let existing = fs::read_to_string(&trends_path)?;
    let updated = match (existing.find(START), existing.find(END)) {
        (Some(s), Some(e)) => {
            let after_end = &existing[e + END.len()..];
            format!("{}{START}{md_rows}{END}{after_end}", &existing[..s])
        }
        _ => return Ok(()), // markers not found, skip
    };
    fs::write(&trends_path, updated)?;
    println!("Updated yearly contributors in {}", trends_path.display());
    Ok(())
}

fn inline_md_to_html(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next(); // consume second *
                let mut inner = String::new();
                loop {
                    match chars.next() {
                        Some('*') if chars.peek() == Some(&'*') => { chars.next(); break; }
                        Some(ch) => inner.push(ch),
                        None => break,
                    }
                }
                result.push_str(&format!("<strong>{}</strong>", html_escape(&inner)));
            }
            '`' => {
                let mut inner = String::new();
                loop {
                    match chars.next() {
                        Some('`') => break,
                        Some(ch) => inner.push(ch),
                        None => break,
                    }
                }
                result.push_str(&format!("<code>{}</code>", html_escape(&inner)));
            }
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            other => result.push(other),
        }
    }
    result
}

fn render_html(repo_display: &str, sections: &[(&str, Vec<ChartEntry>)], notes_html: Option<&str>, github_url: &str) -> String {
    let mut body = String::new();
    if let Some(notes) = notes_html {
        body.push_str(notes);
        body.push('\n');
    }
    let mut inits = String::new();
    let mut n = 0usize;

    for (section_title, entries) in sections {
        if entries.is_empty() { continue; }
        body.push_str(&format!(
            "<div class=\"section-title\">{}</div>\n",
            html_escape(section_title)
        ));
        for entry in entries {
            n += 1;
            body.push_str(&format!(
                "<div class=\"card\"><h3 class=\"chart-title\">{}</h3><div id=\"c{n}\" data-chart style=\"height:{}px;\"></div>",
                html_escape(&entry.title),
                entry.height_px
            ));
            if let Some(note) = &entry.note {
                body.push_str(&format!("<p class=\"chart-note\">{}</p>", html_escape(note)));
            }
            if let Some(extra) = &entry.extra_html {
                body.push_str(extra);
            }
            body.push_str("</div>\n");
            inits.push_str(&format!(
                "echarts.init(document.getElementById('c{n}')).setOption({});\n",
                entry.json
            ));
        }
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{repo} — Trends</title>
  <script src="https://cdn.jsdelivr.net/npm/echarts@5.5.0/dist/echarts.min.js"></script>
  <style>
    * {{ box-sizing: border-box; }}
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; margin: 0; padding: 0; background: #f6f8fa; color: #24292f; }}
    .header {{ background: white; border-bottom: 1px solid #d0d7de; padding: 16px 24px; display: flex; align-items: baseline; gap: 16px; }}
    .header h1 {{ margin: 0; font-size: 1.5em; }}
    .header h1 a {{ color: #24292f; text-decoration: none; }}
    .header h1 a:hover {{ text-decoration: underline; }}
    .header a {{ color: #0969da; text-decoration: none; font-size: 0.9em; }}
    .content {{ max-width: 1200px; margin: 0 auto; padding: 24px; }}
    .section-title {{ font-size: 1.25em; font-weight: 600; border-bottom: 1px solid #d0d7de; padding-bottom: 8px; margin: 32px 0 16px 0; }}
    .card {{ background: white; border: 1px solid #d0d7de; border-radius: 6px; padding: 16px; margin-bottom: 16px; }}
    .chart-title {{ margin: 0 0 12px 0; font-size: 1em; font-weight: 600; color: #24292f; }}
    .chart-note {{ color: #656d76; font-size: 0.85em; margin: 8px 0 0 0; font-style: italic; }}
    .stats-meta {{ color: #656d76; font-size: 0.85em; margin: 16px 0 8px 0; }}
    .stats-table {{ border-collapse: collapse; font-size: 0.9em; margin-bottom: 16px; }}
    .stats-table th, .stats-table td {{ border: 1px solid #d0d7de; padding: 6px 12px; text-align: right; }}
    .stats-table th:first-child, .stats-table td:first-child {{ text-align: left; }}
    .stats-table thead {{ background: #f6f8fa; }}
    .stats-label {{ font-weight: 600; margin: 12px 0 4px 0; font-size: 0.9em; }}
    .notes {{ background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 6px; padding: 16px; margin-bottom: 24px; font-size: 0.9em; }}
    .notes h3 {{ margin-top: 0; }}
  </style>
</head>
<body>
  <div class="header">
    <a href="../index.html">← overview</a>
    <h1><a href="{github_url}">{repo}</a> — Trends</h1>
  </div>
  <div class="content">
    {body}
  </div>
  <script>
    {inits}
    window.addEventListener('resize', function() {{
      document.querySelectorAll('[data-chart]').forEach(function(el) {{
        var inst = echarts.getInstanceByDom(el);
        if (inst) inst.resize();
      }});
    }});
  </script>
</body>
</html>"#,
        repo = html_escape(repo_display),
        github_url = github_url,
        body = body,
        inits = inits,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_index_html(repos: &[(String, String)]) -> String {
    // Group by org, vectordotdev first then alphabetical
    let mut by_org: std::collections::BTreeMap<String, Vec<&(String, String)>> = std::collections::BTreeMap::new();
    for r in repos {
        by_org.entry(r.0.clone()).or_default().push(r);
    }
    // Sort orgs with vectordotdev first
    let mut orgs: Vec<String> = by_org.keys().cloned().collect();
    orgs.sort_by(|a, b| {
        if a == "vectordotdev" { std::cmp::Ordering::Less }
        else if b == "vectordotdev" { std::cmp::Ordering::Greater }
        else { a.cmp(b) }
    });

    let mut body = String::new();
    for org in &orgs {
        let entries = &by_org[org];
        body.push_str(&format!("<h2>{}</h2><ul>\n", html_escape(org)));
        for (_, name) in entries.iter() {
            body.push_str(&format!(
                "<li><a href=\"{name}/index.html\">{name}</a></li>\n",
                name = html_escape(name)
            ));
        }
        body.push_str("</ul>\n");
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>GitHub Tools — Dashboard</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; margin: 0; padding: 24px; background: #f6f8fa; color: #24292f; }}
    h1 {{ font-size: 1.5em; }}
    h2 {{ font-size: 1.1em; color: #656d76; margin: 24px 0 8px 0; font-weight: 600; }}
    ul {{ list-style: none; padding: 0; margin: 0; }}
    li {{ margin: 6px 0; }}
    a {{ color: #0969da; text-decoration: none; font-size: 1.05em; }}
    a:hover {{ text-decoration: underline; }}
  </style>
</head>
<body>
  <h1>Repository Dashboards</h1>
  {body}
</body>
</html>"#,
        body = body
    )
}

// ── Main entry point ─────────────────────────────────────────────────────────

/// Generate charts for a single repo.
/// `repo` is in the form "owner/name" (e.g. "vectordotdev/vector").
pub fn run(input_dir: &str, repo: &str, output_dir: &str, start: Option<&str>) -> Result<()> {
    use chrono::{Datelike, Utc};
    let two_years_ago = {
        let now = Utc::now();
        let total_months = now.year() * 12 + now.month() as i32 - 25;
        format!("{}-{:02}", total_months / 12, total_months % 12 + 1)
    };
    let start = start.or(Some(two_years_ago.as_str()));

    let (owner, name) = repo.split_once('/')
        .with_context(|| format!("repo must be owner/name, got '{repo}'"))?;
    let prefix = format!("{owner}_{name}");
    let repo_display = format!("{owner}/{name}");

    // ── Load CSVs ──
    let issues_monthly = read_csv(&format!("{input_dir}/{prefix}_issues.monthly_summary.csv"))?;
    let pr_monthly = read_csv(&format!("{input_dir}/{prefix}_pull_requests.monthly_summary.csv"))?;
    let disc_monthly = read_csv(&format!("{input_dir}/{prefix}_discussions.monthly_summary.csv"))?;
    let issues_label_bd = read_csv(&format!("{input_dir}/{prefix}_issues.label_breakdown.csv"))?;
    let pr_label_bd = read_csv(&format!("{input_dir}/{prefix}_pull_requests.label_breakdown.csv"))?;
    let issues_label_counts = read_csv(&format!("{input_dir}/{prefix}_issues.label_counts.csv"))?;
    let pr_label_counts = read_csv(&format!("{input_dir}/{prefix}_pull_requests.label_counts.csv"))?;
    let issues_open_by_label = read_csv(&format!("{input_dir}/{prefix}_issues.open_by_label.csv"))?;
    let pr_open_by_label = read_csv(&format!("{input_dir}/{prefix}_pull_requests.open_by_label.csv"))?;
    let pr_contributor: Vec<HashMap<String, String>> = read_csv(&format!("{input_dir}/{prefix}_pull_requests.contributor_monthly.csv"))?
        .into_iter()
        .filter(|r| !r.get("user_login").map(|u| is_bot(u)).unwrap_or(false))
        .collect();

    // ── Derive column lists ──
    let issues_monthly_cols: Vec<String> = if let Some(first) = issues_monthly.first() {
        first.keys().cloned().collect()
    } else { vec![] };
    let pr_monthly_cols: Vec<String> = if let Some(first) = pr_monthly.first() {
        first.keys().cloned().collect()
    } else { vec![] };

    // Top 5 label columns for overlay (those that are not the standard cols)
    let _standard_issue_cols = ["month", "created_issues", "closed_issues"];
    // Fixed type overlays matching Python's TYPE_OVERLAYS.
    // Collects ALL matching columns per type and sums them (mirrors df[matching].sum(axis=1)),
    // so repos that use both "type: bug" (label) and "Bug" (native type) get a continuous series.
    const TYPE_OVERLAY_CANDIDATES: &[(&str, &[&str])] = &[
        ("type: bug",         &["type: bug",         "Bug"]),
        ("type: feature",     &["type: feature",     "Feature"]),
        ("type: enhancement", &["type: enhancement", "Enhancement"]),
        ("type: task",        &["type: task",        "Task"]),
    ];
    fn resolve_type_overlays(cols: &[String]) -> Vec<(String, Vec<String>)> {
        TYPE_OVERLAY_CANDIDATES.iter()
            .filter_map(|(name, candidates)| {
                let matching: Vec<String> = candidates.iter()
                    .filter(|&&c| cols.iter().any(|col| col == c))
                    .map(|&c| c.to_string())
                    .collect();
                if matching.is_empty() { None } else { Some((name.to_string(), matching)) }
            })
            .collect()
    }

    let issue_type_overlays = resolve_type_overlays(&issues_monthly_cols);
    let pr_type_overlays = resolve_type_overlays(&pr_monthly_cols);

    // ── Build chart sections ──
    let mut sections: Vec<(&str, Vec<ChartEntry>)> = Vec::new();

    // Issues section
    {
        let mut entries = Vec::new();
        if !issues_monthly.is_empty() {
            let filtered = filter_by_start(&issues_monthly, start);
            let chart = monthly_trend(
                &filtered,
                "created_issues",
                "closed_issues",
                &issue_type_overlays,
            );
            entries.push(chart_to_entry("Monthly Trend (Issues)", None, &chart)?);
        }
        if !issues_label_bd.is_empty() {
            let filtered = filter_by_start(&issues_label_bd, None);
            let chart = label_breakdown(&filtered);
            entries.push(chart_to_entry_h("Top Labels (Issues)", Some("no-changelog and meta: awaiting author excluded"), &chart, 550)?);
        }
        if !issues_label_counts.is_empty() {
            let filtered = filter_by_start(&issues_label_counts, start);
            let chart = label_counts_over_time(&filtered);
            entries.push(chart_to_entry("Label Counts Over Time (Issues)", Some("no-changelog and meta: awaiting author excluded"), &chart)?);
        }
        if !issues_open_by_label.is_empty() {
            let filtered = filter_by_start(&issues_open_by_label, None);
            if let Some(chart) = open_closed_by_label(&filtered) {
                entries.push(chart_to_entry_h("Top Integrations: Open vs Closed (Issues)", None, &chart, 650)?);
            }
        }
        if let Some(chart) = integration_trends(
            &filter_by_start(&issues_monthly, start),
            &issues_monthly_cols,
        ) {
            entries.push(chart_to_entry("Integration Trends (Issues)", None, &chart)?);
        }
        if !entries.is_empty() {
            sections.push(("Issues", entries));
        }
    }

    // Pull Requests section
    {
        let mut entries = Vec::new();
        if !pr_monthly.is_empty() {
            let filtered = filter_by_start(&pr_monthly, start);
            let chart = monthly_trend(
                &filtered,
                "created_pull_requests",
                "closed_pull_requests",
                &pr_type_overlays,
            );
            entries.push(chart_to_entry("Monthly Trend (Pull Requests)", Some("Draft PRs excluded"), &chart)?);
        }
        if !pr_label_bd.is_empty() {
            let filtered = filter_by_start(&pr_label_bd, None);
            let chart = label_breakdown(&filtered);
            entries.push(chart_to_entry_h("Top Labels (Pull Requests)", Some("Draft PRs excluded · no-changelog and meta: awaiting author excluded"), &chart, 550)?);
        }
        if !pr_label_counts.is_empty() {
            let filtered = filter_by_start(&pr_label_counts, start);
            let chart = label_counts_over_time(&filtered);
            entries.push(chart_to_entry("Label Counts Over Time (Pull Requests)", Some("Draft PRs excluded · no-changelog and meta: awaiting author excluded"), &chart)?);
        }
        if !pr_open_by_label.is_empty() {
            let filtered = filter_by_start(&pr_open_by_label, None);
            if let Some(chart) = open_closed_by_label(&filtered) {
                entries.push(chart_to_entry_h("Top Integrations: Open vs Closed (Pull Requests)", Some("Draft PRs excluded"), &chart, 650)?);
            }
        }
        if let Some(chart) = integration_trends(
            &filter_by_start(&pr_monthly, start),
            &pr_monthly_cols,
        ) {
            entries.push(chart_to_entry("Integration Trends (Pull Requests)", Some("Draft PRs excluded"), &chart)?);
        }
        if !entries.is_empty() {
            sections.push(("Pull Requests", entries));
        }
    }

    // Discussions section
    if !disc_monthly.is_empty() {
        let filtered = filter_by_start(&disc_monthly, start);
        let chart = discussion_trend(&filtered);
        let entry = chart_to_entry("Monthly Trend (Discussions)", None, &chart)?;
        sections.push(("Discussions", vec![entry]));
    }

    // Contributors section
    if !pr_contributor.is_empty() {
        let mut entries = Vec::new();
        let filtered_contrib = filter_by_start(&pr_contributor, start);
        if let Some(chart) = contributor_heatmap(&filtered_contrib) {
            entries.push(chart_to_entry_h("Contributor Heatmap", Some("Last 12 months, top 10 contributors · draft PRs and known bot accounts excluded"), &chart, 350)?);
        }
        if let Some(chart) = unique_contributors_monthly(&filtered_contrib) {
            entries.push(chart_to_entry("Contributors Monthly", Some("Draft PRs and known bot accounts excluded"), &chart)?);
        }
        let all_contrib: Vec<&HashMap<String, String>> = pr_contributor.iter().collect();

        // Compute yearly stats (used for both HTML table and markdown write-back)
        let mut yearly_stats: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        {
            let mut seen_users: HashSet<String> = HashSet::new();
            // Group by year, preserving chronological order
            let mut by_year: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for row in &all_contrib {
                let month = match row.get("month") { Some(m) => m, None => continue };
                let user = match row.get("user_login") { Some(u) => u.clone(), None => continue };
                let year = month[..4.min(month.len())].to_string();
                by_year.entry(year).or_default().push(user);
            }
            for (year, users) in &by_year {
                let mut new_count = 0i64;
                let mut ret_count = 0i64;
                // Deduplicate users within the year before classifying
                let unique_users: HashSet<String> = users.iter().cloned().collect();
                for user in &unique_users {
                    if seen_users.contains(user) {
                        ret_count += 1;
                    } else {
                        new_count += 1;
                        seen_users.insert(user.clone());
                    }
                }
                yearly_stats.insert(year.clone(), (new_count, ret_count));
            }
        }

        if let Some(chart) = unique_contributors_yearly(&all_contrib) {
            use chrono::{Datelike, Utc};
            let current_year = Utc::now().year().to_string();
            let current_month = Utc::now().month();
            let mut table_rows = String::new();
            for (year, (new_c, ret_c)) in &yearly_stats {
                let unique = new_c + ret_c;
                let year_label = if year.as_str() == current_year {
                    format!("{year} (YTD, {}mo)", current_month - 1)
                } else {
                    year.to_string()
                };
                table_rows.push_str(&format!(
                    "<tr><td>{year_label}</td><td>{unique}</td><td>{new_c}</td><td>{ret_c}</td></tr>"
                ));
            }
            let yearly_table = format!(
                "<p class=\"stats-label\">Unique PR contributors by year</p>\
                <table class=\"stats-table\">\
                <thead><tr><th>Year</th><th>Unique</th><th>New</th><th>Returning</th></tr></thead>\
                <tbody>{table_rows}</tbody>\
                </table>"
            );
            let mut yearly_entry = chart_to_entry_h("Contributors Yearly", Some("Draft PRs and known bot accounts excluded"), &chart, 500)?;
            yearly_entry.extra_html = Some(yearly_table);
            entries.push(yearly_entry);
        }
        if !entries.is_empty() {
            sections.push(("Contributors", entries));
        }
        if !pr_contributor.is_empty() {
            update_yearly_contributors_md(name, &yearly_stats)?;
        }
    }

    // AI Code Review section
    if let Some(stats) = read_ai_stats(input_dir, &prefix) {
        let entry = ai_stats_entry(&stats)?;
        sections.push(("AI Code Review", vec![entry]));
    }

    // ── Write output ──
    let repo_out_dir = format!("{output_dir}/{name}");
    fs::create_dir_all(&repo_out_dir)
        .with_context(|| format!("creating directory {repo_out_dir}"))?;

    let data_notes = read_data_notes(name);
    let github_url = format!("https://github.com/{owner}/{name}");
    let html = render_html(&repo_display, &sections, data_notes.as_deref(), &github_url);
    let out_path = format!("{repo_out_dir}/index.html");
    fs::write(&out_path, &html)
        .with_context(|| format!("writing {out_path}"))?;

    println!("Generated: {out_path}");

    // Write .repo marker so the index knows the full owner/name
    let repo_marker = Path::new(output_dir).join(name).join(".repo");
    fs::write(&repo_marker, format!("{owner}/{name}"))?;

    // Regenerate the top-level index by scanning for subdirectories with .repo
    let mut repos_found: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = fs::read_dir(output_dir) {
        let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        sorted.sort_by_key(|e| e.file_name());
        for entry in sorted {
            let path = entry.path();
            let repo_file = path.join(".repo");
            if path.is_dir() && repo_file.exists()
                && let Ok(content) = fs::read_to_string(&repo_file)
                    && let Some((o, n)) = content.trim().split_once('/') {
                        repos_found.push((o.to_string(), n.to_string()));
                    }
        }
    }
    generate_index(output_dir, &repos_found)?;

    Ok(())
}

/// Generate the overview index.html listing all repos.
pub fn generate_index(output_dir: &str, repos: &[(String, String)]) -> Result<()> {
    let html = render_index_html(repos);
    let out_path = format!("{output_dir}/index.html");
    fs::write(&out_path, &html)
        .with_context(|| format!("writing {out_path}"))?;
    println!("Generated index: {out_path}");
    Ok(())
}
