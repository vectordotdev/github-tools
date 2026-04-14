use crate::config::Config;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
use std::fs;
use std::path::Path;

pub fn run(db: &str, config: &Config, exclude_labels: Option<&str>) -> Result<()> {
    let conn = Connection::open(db).with_context(|| format!("Failed to open database: {db}"))?;

    let out_dir = Path::new("out/summaries");
    fs::create_dir_all(out_dir)?;

    let exclude: Option<Vec<String>> = exclude_labels.map(|s| {
        s.split(',')
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    });
    let exc = exclude.as_deref();

    for table in &["issues", "pull_requests"] {
        println!("Generating summaries for table '{table}'...");
        export_open_by_label(&conn, out_dir, config, table, exc)?;
        export_monthly_summary(&conn, out_dir, config, table, exc)?;
        export_label_breakdown(&conn, out_dir, config, table, exc)?;
        export_label_timeseries(&conn, out_dir, config, table, exc)?;
        export_overall_totals(&conn, out_dir, config, table, exc)?;
    }

    // Generate discussion summaries if the table exists
    let has_discussions: bool = conn
        .prepare("SELECT 1 FROM discussions LIMIT 1")
        .and_then(|mut s| s.query_row([], |_| Ok(true)))
        .unwrap_or(false);
    if has_discussions {
        println!("Generating summaries for discussions...");
        export_discussion_monthly_summary(&conn, out_dir, config)?;
    }

    println!("Done. All CSVs saved to '{}'", out_dir.display());
    Ok(())
}

/// Build the WHERE clause parts and params for excluding items with certain labels.
/// Returns (sql_condition, params) where sql_condition is like
/// "{table}.id NOT IN (SELECT ...)" and params are the label names.
fn exclude_filter(table: &str, exclude_labels: Option<&[String]>) -> (Option<String>, Vec<String>) {
    match exclude_labels {
        Some(labels) if !labels.is_empty() => {
            let placeholders = labels.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let sql = format!(
                "{table}.id NOT IN (
                    SELECT issue_id FROM issue_labels
                    JOIN labels ON labels.id = issue_labels.label_id
                    WHERE labels.name IN ({placeholders})
                )"
            );
            let mut params = labels.to_vec();
            params.sort();
            (Some(sql), params)
        }
        _ => (None, Vec::new()),
    }
}

/// Combine the base where clause (e.g. draft filter), exclude filter, and any extra conditions.
fn build_where(table: &str, exclude_labels: Option<&[String]>, extra: &[&str]) -> (String, Vec<String>) {
    let mut parts: Vec<String> = Vec::new();

    if table == "pull_requests" {
        parts.push("is_draft = 0".to_string());
    }

    let (exc_sql, params) = exclude_filter(table, exclude_labels);
    if let Some(sql) = exc_sql {
        parts.push(sql);
    }

    for cond in extra {
        parts.push(cond.to_string());
    }

    let where_sql = if parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", parts.join(" AND "))
    };

    (where_sql, params)
}

fn csv_path(out_dir: &Path, config: &Config, table: &str, suffix: &str) -> std::path::PathBuf {
    out_dir.join(format!(
        "{}_{}_{}.{}.csv",
        config.repo_owner, config.repo_name, table, suffix
    ))
}

fn to_rusqlite_params(params: &[String]) -> Vec<rusqlite::types::Value> {
    params
        .iter()
        .map(|s| rusqlite::types::Value::Text(s.clone()))
        .collect()
}

fn export_monthly_summary(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
    exclude_labels: Option<&[String]>,
) -> Result<()> {
    let (wc, params) = build_where(table, exclude_labels, &[]);
    let (wc_closed, params_closed) = build_where(table, exclude_labels, &["closed_at IS NOT NULL"]);

    let current_month = Utc::now().format("%Y-%m").to_string();
    eprintln!("Warning: excluding current incomplete month ({current_month}) from monthly summary for {table}");

    // Get all distinct label names for this table (respecting exclude filter)
    let label_names: Vec<String> = {
        let label_query = format!(
            "SELECT DISTINCT labels.name
             FROM issue_labels
             JOIN labels ON labels.id = issue_labels.label_id
             JOIN {table} ON {table}.id = issue_labels.issue_id
             {wc}"
        );
        let rp = to_rusqlite_params(&params);
        let mut stmt = conn.prepare(&label_query)?;
        stmt.query_map(rusqlite::params_from_iter(rp.iter()), |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };

    let label_columns_sql = label_names
        .iter()
        .map(|l| format!("SUM(CASE WHEN lc.label_name = '{l}' THEN 1 ELSE 0 END) AS \"{l}\""))
        .collect::<Vec<_>>()
        .join(",\n        ");

    let maybe_label_cols = if label_columns_sql.is_empty() {
        String::new()
    } else {
        format!(",\n            {label_columns_sql}")
    };

    // Add issue_type breakdown columns if the table has an issue_type column
    let has_issue_type = conn
        .prepare(&format!("SELECT issue_type FROM {table} LIMIT 0"))
        .is_ok();
    let issue_type_names: Vec<String> = if has_issue_type {
        let (type_wc, type_params) = build_where(table, exclude_labels, &["issue_type IS NOT NULL"]);
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT issue_type FROM {table} {type_wc} ORDER BY issue_type"
        ))?;
        let rp = to_rusqlite_params(&type_params);
        stmt.query_map(rusqlite::params_from_iter(rp.iter()), |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    let maybe_type_cols = if issue_type_names.is_empty() {
        String::new()
    } else {
        let cols = issue_type_names
            .iter()
            .map(|t| format!("SUM(CASE WHEN it.issue_type = '{t}' AND e.event = 'created' THEN 1 ELSE 0 END) AS \"{t}\""))
            .collect::<Vec<_>>()
            .join(",\n            ");
        format!(",\n            {cols}")
    };

    let maybe_type_join = if issue_type_names.is_empty() {
        String::new()
    } else {
        format!("\n        LEFT JOIN {table} it ON e.issue_id = it.id")
    };

    // Combine params: created query params + closed query params
    let mut all_params = params.clone();
    all_params.extend(params_closed);
    // + one more copy for label_counts CTE
    all_params.extend(params);

    let query = format!(
        "WITH events AS (
            SELECT substr(created_at, 1, 7) AS month, id AS issue_id, 'created' AS event
            FROM {table} {wc}
            UNION ALL
            SELECT substr(closed_at, 1, 7) AS month, id AS issue_id, 'closed' AS event
            FROM {table} {wc_closed}
        ),
        label_counts AS (
            SELECT issue_labels.issue_id, labels.name AS label_name
            FROM issue_labels
            JOIN labels ON labels.id = issue_labels.label_id
            JOIN {table} ON {table}.id = issue_labels.issue_id
            {wc}
        )
        SELECT
            e.month,
            SUM(CASE WHEN e.event = 'created' THEN 1 ELSE 0 END) AS created_{table},
            SUM(CASE WHEN e.event = 'closed' THEN 1 ELSE 0 END) AS closed_{table}{maybe_label_cols}{maybe_type_cols}
        FROM events e
        LEFT JOIN label_counts lc ON e.issue_id = lc.issue_id{maybe_type_join}
        GROUP BY e.month
        HAVING e.month < '{current_month}'
        ORDER BY e.month"
    );

    write_query_to_csv(
        conn,
        &query,
        &to_rusqlite_params(&all_params),
        &csv_path(out_dir, config, table, "monthly_summary"),
    )
}

fn export_label_breakdown(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
    exclude_labels: Option<&[String]>,
) -> Result<()> {
    let (wc, params) = build_where(table, exclude_labels, &[]);
    let query = format!(
        "SELECT labels.name AS label_name, COUNT(*) AS count
         FROM issue_labels
         JOIN labels ON labels.id = issue_labels.label_id
         JOIN {table} ON {table}.id = issue_labels.issue_id
         {wc}
         GROUP BY labels.name
         ORDER BY count DESC"
    );
    write_query_to_csv(
        conn,
        &query,
        &to_rusqlite_params(&params),
        &csv_path(out_dir, config, table, "label_breakdown"),
    )
}

fn export_label_timeseries(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
    exclude_labels: Option<&[String]>,
) -> Result<()> {
    let (wc, params) = build_where(table, exclude_labels, &[]);
    let query = format!(
        "SELECT substr({table}.created_at, 1, 7) AS month, labels.name AS label_name, COUNT(*) AS count
         FROM {table}
         JOIN issue_labels ON {table}.id = issue_labels.issue_id
         JOIN labels ON labels.id = issue_labels.label_id
         {wc}
         GROUP BY month, label_name
         ORDER BY month, count DESC"
    );
    write_query_to_csv(
        conn,
        &query,
        &to_rusqlite_params(&params),
        &csv_path(out_dir, config, table, "label_counts"),
    )
}

fn export_open_by_label(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
    exclude_labels: Option<&[String]>,
) -> Result<()> {
    let (wc, params) = build_where(table, exclude_labels, &[]);
    let query = format!(
        "SELECT labels.name AS label_name,
                SUM(CASE WHEN {table}.state = 'open' THEN 1 ELSE 0 END) AS open_count,
                SUM(CASE WHEN {table}.state = 'closed' THEN 1 ELSE 0 END) AS closed_count
         FROM {table}
         JOIN issue_labels ON {table}.id = issue_labels.issue_id
         JOIN labels ON labels.id = issue_labels.label_id
         {wc}
         GROUP BY labels.name
         ORDER BY open_count DESC, closed_count DESC"
    );
    write_query_to_csv(
        conn,
        &query,
        &to_rusqlite_params(&params),
        &csv_path(out_dir, config, table, "open_by_label"),
    )
}

fn export_overall_totals(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
    exclude_labels: Option<&[String]>,
) -> Result<()> {
    let (wc, params) = build_where(table, exclude_labels, &[]);
    let query = format!(
        "SELECT
            SUM(CASE WHEN state = 'open'   THEN 1 ELSE 0 END) AS total_open_{table},
            SUM(CASE WHEN state = 'closed' THEN 1 ELSE 0 END) AS total_closed_{table}
         FROM {table} {wc}"
    );

    write_query_to_csv(
        conn,
        &query,
        &to_rusqlite_params(&params),
        &csv_path(out_dir, config, table, "overall_totals"),
    )
}

fn export_discussion_monthly_summary(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
) -> Result<()> {
    let current_month = Utc::now().format("%Y-%m").to_string();

    let query = format!(
        "SELECT
            substr(created_at, 1, 7) AS month,
            COUNT(*) AS created_discussions,
            SUM(CASE WHEN closed THEN 1 ELSE 0 END) AS closed_discussions,
            SUM(CASE WHEN is_answered THEN 1 ELSE 0 END) AS answered_discussions
         FROM discussions
         GROUP BY month
         HAVING month < '{current_month}'
         ORDER BY month"
    );

    write_query_to_csv(
        conn,
        &query,
        &[],
        &csv_path(out_dir, config, "discussions", "monthly_summary"),
    )
}

fn write_query_to_csv(
    conn: &Connection,
    query: &str,
    params: &[rusqlite::types::Value],
    path: &Path,
) -> Result<()> {
    let mut stmt = conn.prepare(query)?;
    let headers: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    let col_count = headers.len();
    let rows: Vec<Vec<String>> = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let mut values = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let val: rusqlite::types::Value = row.get(i)?;
                values.push(value_to_string(val));
            }
            Ok(values)
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut wtr = csv::Writer::from_path(path)
        .with_context(|| format!("Failed to create CSV at {}", path.display()))?;
    wtr.write_record(&headers)?;
    for row in rows {
        wtr.write_record(&row)?;
    }
    wtr.flush()?;
    println!("  Wrote {}", path.display());
    Ok(())
}

fn value_to_string(val: rusqlite::types::Value) -> String {
    match val {
        rusqlite::types::Value::Null => String::new(),
        rusqlite::types::Value::Integer(i) => i.to_string(),
        rusqlite::types::Value::Real(f) => f.to_string(),
        rusqlite::types::Value::Text(s) => s,
        rusqlite::types::Value::Blob(b) => String::from_utf8_lossy(&b).to_string(),
    }
}
