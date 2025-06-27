import argparse
import csv
import logging
import os
import sqlite3

from scripts.logging.custom_logging import setup_logger
from scripts.util.load_env import load_github_env_vars

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/summaries"))
os.makedirs(OUTPUT_DIR, exist_ok=True)


def export_monthly_summary(env, cur, table):
    logging.info(f"Executing dynamic monthly summary with all labels for table '{table}'...")
    output_path = os.path.join(OUTPUT_DIR, f"{env["REPO_OWNER"]}_{env["REPO_NAME"]}_{table}.monthly_summary.csv")
    where_clause = "WHERE is_draft = 0" if table == "pull_requests" else ""

    # Step 1: Get all distinct label names used with this table
    cur.execute(f"""
        SELECT DISTINCT labels.name
        FROM issue_labels
        JOIN labels ON labels.id = issue_labels.label_id
        JOIN {table} ON {table}.id = issue_labels.issue_id
        {where_clause}
    """)
    label_names = [row[0] for row in cur.fetchall()]
    logging.info(f"Found {len(label_names)} labels for table '{table}'")

    # Step 2: Build dynamic SUM(CASE ...) blocks for each label
    label_columns_sql = ",\n        ".join(
        [f"SUM(CASE WHEN lc.label_name = '{label}' THEN 1 ELSE 0 END) AS \"{label}\""
         for label in label_names]
    )

    # Step 3: Build final SQL query
    query = f"""
    WITH month_base AS (
        SELECT
            substr(created_at, 1, 7) AS month,
            id AS issue_id,
            state
        FROM {table}
        {where_clause}
    ),
    label_counts AS (
        SELECT
            issue_labels.issue_id,
            labels.name AS label_name
        FROM issue_labels
        JOIN labels ON labels.id = issue_labels.label_id
        JOIN {table} ON {table}.id = issue_labels.issue_id
        {where_clause}
    )
    SELECT
        mb.month,
        SUM(CASE WHEN mb.state = 'open' THEN 1 ELSE 0 END) AS open_{table},
        SUM(CASE WHEN mb.state = 'closed' THEN 1 ELSE 0 END) AS closed_{table},
        {label_columns_sql}
    FROM month_base mb
    LEFT JOIN label_counts lc ON mb.issue_id = lc.issue_id
    GROUP BY mb.month
    ORDER BY mb.month
    """

    cur.execute(query)
    rows = cur.fetchall()
    column_names = [desc[0] for desc in cur.description]

    logging.info(f"Writing expanded monthly summary to {output_path}")
    with open(output_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(column_names)
        writer.writerows(rows)


def export_label_breakdown(env, cur, table):
    logging.info(f"Executing label breakdown query for table '{table}'...")
    output_path = os.path.join(OUTPUT_DIR, f"{env["REPO_OWNER"]}_{env["REPO_NAME"]}_{table}.label_breakdown.csv")
    where_clause = "WHERE is_draft = 0" if table == "pull_requests" else ""

    query = f"""
    SELECT labels.name AS label_name, COUNT(*) AS count
    FROM issue_labels
    JOIN labels ON labels.id = issue_labels.label_id
    JOIN {table} ON {table}.id = issue_labels.issue_id
    {where_clause}
    GROUP BY labels.name
    ORDER BY count DESC
    """
    cur.execute(query)
    rows = cur.fetchall()

    logging.info(f"Writing label breakdown to {output_path}")
    with open(output_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["label_name", "count"])
        writer.writerows(rows)


def export_label_timeseries(env, cur, table):
    logging.info(f"Executing label time-series breakdown query for table '{table}'...")
    output_path = os.path.join(OUTPUT_DIR, f"{env["REPO_OWNER"]}_{env["REPO_NAME"]}_{table}.label_counts.csv")
    where_clause = "WHERE is_draft = 0" if table == "pull_requests" else ""

    query = f"""
    SELECT
        substr({table}.created_at, 1, 7) AS month,
        labels.name AS label_name,
        COUNT(*) AS count
    FROM {table}
    JOIN issue_labels ON {table}.id = issue_labels.issue_id
    JOIN labels ON labels.id = issue_labels.label_id
    {where_clause}
    GROUP BY month, label_name
    ORDER BY month, count DESC
    """
    cur.execute(query)
    rows = cur.fetchall()

    logging.info(f"Writing label time-series to {output_path}")
    with open(output_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["month", "label_name", "count"])
        writer.writerows(rows)


def export_open_by_label(env, cur, table):
    logging.info(f"Calculating open {table} count by label...")
    output_path = os.path.join(OUTPUT_DIR, f"{env["REPO_OWNER"]}_{env["REPO_NAME"]}_{table}.open_by_label.csv")
    where_clause = "WHERE is_draft = 0" if table == "pull_requests" else ""

    query = f"""
       SELECT
           labels.name AS label_name,
           SUM(CASE WHEN {table}.state = 'open' THEN 1 ELSE 0 END) AS open_count,
           SUM(CASE WHEN {table}.state = 'closed' THEN 1 ELSE 0 END) AS closed_count
       FROM {table}
       JOIN issue_labels ON {table}.id = issue_labels.issue_id
       JOIN labels ON labels.id = issue_labels.label_id
       {where_clause}
       GROUP BY labels.name
       ORDER BY open_count DESC, closed_count DESC
       """
    cur.execute(query)
    rows = cur.fetchall()

    logging.info(f"Writing open-by-label breakdown to {output_path}")
    with open(output_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["label_name", "open_count", "closed_count"])
        writer.writerows(rows)


def export_overall_totals(env, cur, table, exclude_labels=None):
    """
    Writes a CSV with two columns—total_open_<table> and total_closed_<table>—
    for the given table name, excluding any items that have labels in exclude_labels.
    """
    # 1) Build a list of WHERE predicates
    where_clauses = []
    params = []

    # pull_requests get filtered for drafts
    if table == "pull_requests":
        where_clauses.append("is_draft = 0")

    # exclude any IDs that have an unwanted label
    if exclude_labels:
        placeholders = ", ".join("?" for _ in exclude_labels)
        where_clauses.append(
            f"""{table}.id NOT IN (
                  SELECT issue_id
                  FROM issue_labels
                  JOIN labels ON labels.id = issue_labels.label_id
                  WHERE labels.name IN ({placeholders})
               )"""
        )
        params.extend(sorted(exclude_labels))

    # 2) Join them into a single WHERE … AND … clause (or omit entirely)
    where_sql = ""
    if where_clauses:
        where_sql = "WHERE " + " AND ".join(where_clauses)

    # 3) Run the query
    query = f"""
        SELECT
            SUM(CASE WHEN state = 'open'   THEN 1 ELSE 0 END) AS total_open_{table},
            SUM(CASE WHEN state = 'closed' THEN 1 ELSE 0 END) AS total_closed_{table}
        FROM {table}
        {where_sql}
    """
    logging.info(f"Executing overall-totals for `{table}` with excludes={exclude_labels!r}")
    cur.execute(query, params)
    total_open, total_closed = cur.fetchone()

    # 4) Dump to CSV
    output_path = os.path.join(
        OUTPUT_DIR,
        f"{env['REPO_OWNER']}_{env['REPO_NAME']}_{table}.overall_totals.csv"
    )
    logging.info(f"Writing overall totals for `{table}` to {output_path}")
    with open(output_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow([f"total_open_{table}", f"total_closed_{table}"])
        writer.writerow([total_open or 0, total_closed or 0])


def parse_args():
    parser = argparse.ArgumentParser(description="Generate GitHub issue summaries from SQLite.")
    parser.add_argument("--db", required=True, help="Path to the SQLite database with issues and labels.")
    parser.add_argument(
        "--env-file",
        type=str,
        help="Path to the .env file to load environment variables from",
    )
    parser.add_argument(
        "--exclude-labels",
        type=str,
        help="Comma-separated list of labels to exclude from the various charts",
    )
    args = parser.parse_args()

    raw = args.exclude_labels
    if raw:
        labels = {lbl.strip() for lbl in raw.split(",") if lbl.strip()}
        args.exclude_labels = labels if labels else None
    else:
        args.exclude_labels = None

    return args


def main():
    setup_logger()

    parser = argparse.ArgumentParser(description="Generate GitHub issue summaries from SQLite.")
    parser.add_argument("--db", required=True, help="Path to the SQLite database with issues and labels.")
    parser.add_argument(
        "--env-file",
        type=str,
        help="Path to the .env file to load environment variables from",
    )
    args = parse_args()

    try:
        env = load_github_env_vars(args.env_file)
    except ValueError as e:
        print(f"Error loading environment variables: {e}")
        return 1

    db_path = args.db
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()

    for table in ["issues", "pull_requests"]:
        export_open_by_label(env, cur, table)
        export_monthly_summary(env, cur, table)
        export_label_breakdown(env, cur, table)
        export_label_timeseries(env, cur, table)
        export_overall_totals(env, cur, table, args.exclude_labels)

    conn.close()
    logging.info("Done. All CSVs saved.")


if __name__ == "__main__":
    main()
