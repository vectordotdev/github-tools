import argparse
import sqlite3
import csv
import os
import logging

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/summaries"))
os.makedirs(OUTPUT_DIR, exist_ok=True)


def export_monthly_summary(cur, table):
    logging.info(f"Executing monthly summary query for table '{table}'...")
    output_path = os.path.join(OUTPUT_DIR, f"monthly_summary.{table}.csv")

    query = f"""
    WITH month_base AS (
        SELECT
            substr(created_at, 1, 7) AS month,
            id AS issue_id,
            state
        FROM {table}
    ),
    label_counts AS (
        SELECT
            issue_id,
            labels.name AS label_name
        FROM issue_labels
        JOIN labels ON labels.id = issue_labels.label_id
        WHERE labels.name IN ('type: bug', 'type: feature', 'type: enhancement')
    )
    SELECT
        mb.month,
        SUM(CASE WHEN mb.state = 'open' THEN 1 ELSE 0 END) AS open_issues,
        SUM(CASE WHEN mb.state = 'closed' THEN 1 ELSE 0 END) AS closed_issues,
        SUM(CASE WHEN lc.label_name = 'type: bug' THEN 1 ELSE 0 END) AS bugs,
        SUM(CASE WHEN lc.label_name = 'type: feature' THEN 1 ELSE 0 END) AS features,
        SUM(CASE WHEN lc.label_name = 'type: enhancement' THEN 1 ELSE 0 END) AS enhancements
    FROM month_base mb
    LEFT JOIN label_counts lc ON mb.issue_id = lc.issue_id
    GROUP BY mb.month
    ORDER BY mb.month
    """
    cur.execute(query)
    rows = cur.fetchall()

    logging.info(f"Writing monthly summary to {output_path}")
    with open(output_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["month", "open_issues", "closed_issues", "bugs", "features", "enhancements"])
        writer.writerows(rows)


def export_label_breakdown(cur, table):
    logging.info(f"Executing label breakdown query for table '{table}'...")
    output_path = os.path.join(OUTPUT_DIR, f"label_breakdown.{table}.csv")

    query = """
    SELECT labels.name AS label_name, COUNT(*) AS count
    FROM issue_labels
    JOIN labels ON labels.id = issue_labels.label_id
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


def export_label_timeseries(cur, table):
    logging.info(f"Executing label time-series breakdown query for table '{table}'...")
    output_path = os.path.join(OUTPUT_DIR, f"label_counts.{table}.csv")

    query = f"""
    SELECT
        substr({table}.created_at, 1, 7) AS month,
        labels.name AS label_name,
        COUNT(*) AS count
    FROM {table}
    JOIN issue_labels ON {table}.id = issue_labels.issue_id
    JOIN labels ON labels.id = issue_labels.label_id
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


def main():
    parser = argparse.ArgumentParser(description="Generate GitHub issue summaries from SQLite.")
    parser.add_argument("--db", required=True, help="Path to the SQLite database with issues and labels.")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(levelname)s: %(message)s")

    db_path = args.db
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()

    for table in ["issues", "pull_requests"]:
        export_monthly_summary(cur, table)
        export_label_breakdown(cur, table)
        export_label_timeseries(cur, table)

    conn.close()
    logging.info("Done. All CSVs saved.")


if __name__ == "__main__":
    main()
