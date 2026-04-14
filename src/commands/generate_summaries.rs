use crate::config::Config;
use anyhow::{Context, Result};
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

    for table in &["issues", "pull_requests"] {
        println!("Generating summaries for table '{table}'...");
        export_open_by_label(&conn, out_dir, config, table)?;
        export_monthly_summary(&conn, out_dir, config, table)?;
        export_label_breakdown(&conn, out_dir, config, table)?;
        export_label_timeseries(&conn, out_dir, config, table)?;
        export_overall_totals(&conn, out_dir, config, table, exclude.as_deref())?;
    }

    println!("Done. All CSVs saved to '{}'", out_dir.display());
    Ok(())
}

fn where_clause(table: &str) -> &'static str {
    if table == "pull_requests" {
        "WHERE is_draft = 0"
    } else {
        ""
    }
}

fn csv_path(out_dir: &Path, config: &Config, table: &str, suffix: &str) -> std::path::PathBuf {
    out_dir.join(format!(
        "{}_{}_{}.{}.csv",
        config.repo_owner, config.repo_name, table, suffix
    ))
}

fn export_monthly_summary(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
) -> Result<()> {
    let wc = where_clause(table);
    let where_and_owned = if wc.is_empty() {
        "WHERE".to_string()
    } else {
        format!("{wc} AND")
    };
    let where_and = where_and_owned.as_str();

    // Step 1: get all distinct label names for this table
    let label_names: Vec<String> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT labels.name
             FROM issue_labels
             JOIN labels ON labels.id = issue_labels.label_id
             JOIN {table} ON {table}.id = issue_labels.issue_id
             {wc}"
        ))?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };

    // Step 2: build dynamic SUM(CASE ...) columns
    let label_columns_sql = label_names
        .iter()
        .map(|l| format!("SUM(CASE WHEN lc.label_name = '{l}' THEN 1 ELSE 0 END) AS \"{l}\""))
        .collect::<Vec<_>>()
        .join(",\n        ");

    // Use a UNION of created and closed events so each month reflects activity:
    // - created_{table}: items opened that month
    // - closed_{table}: items closed/merged that month (by closed_at)
    let query = format!(
        "WITH events AS (
            SELECT substr(created_at, 1, 7) AS month, id AS issue_id, 'created' AS event
            FROM {table} {wc}
            UNION ALL
            SELECT substr(closed_at, 1, 7) AS month, id AS issue_id, 'closed' AS event
            FROM {table} {where_and} closed_at IS NOT NULL
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
            SUM(CASE WHEN e.event = 'closed' THEN 1 ELSE 0 END) AS closed_{table},
            {label_columns_sql}
        FROM events e
        LEFT JOIN label_counts lc ON e.issue_id = lc.issue_id
        GROUP BY e.month
        ORDER BY e.month"
    );

    write_query_to_csv(
        conn,
        &query,
        &[],
        &csv_path(out_dir, config, table, "monthly_summary"),
    )
}

fn export_label_breakdown(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
) -> Result<()> {
    let wc = where_clause(table);
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
        &[],
        &csv_path(out_dir, config, table, "label_breakdown"),
    )
}

fn export_label_timeseries(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
) -> Result<()> {
    let wc = where_clause(table);
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
        &[],
        &csv_path(out_dir, config, table, "label_counts"),
    )
}

fn export_open_by_label(
    conn: &Connection,
    out_dir: &Path,
    config: &Config,
    table: &str,
) -> Result<()> {
    let wc = where_clause(table);
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
        &[],
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
    let mut where_parts: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();

    if table == "pull_requests" {
        where_parts.push("is_draft = 0".to_string());
    }

    if let Some(labels) = exclude_labels
        && !labels.is_empty() {
            let placeholders = labels.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            where_parts.push(format!(
                "{table}.id NOT IN (
                    SELECT issue_id FROM issue_labels
                    JOIN labels ON labels.id = issue_labels.label_id
                    WHERE labels.name IN ({placeholders})
                )"
            ));
            let mut sorted = labels.to_vec();
            sorted.sort();
            params.extend(sorted);
        }

    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    let query = format!(
        "SELECT
            SUM(CASE WHEN state = 'open'   THEN 1 ELSE 0 END) AS total_open_{table},
            SUM(CASE WHEN state = 'closed' THEN 1 ELSE 0 END) AS total_closed_{table}
         FROM {table} {where_sql}"
    );

    let rusqlite_params: Vec<rusqlite::types::Value> = params
        .into_iter()
        .map(rusqlite::types::Value::Text)
        .collect();

    write_query_to_csv(
        conn,
        &query,
        &rusqlite_params,
        &csv_path(out_dir, config, table, "overall_totals"),
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
