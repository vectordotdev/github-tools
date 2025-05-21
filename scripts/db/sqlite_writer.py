import argparse
import json
import os
import sqlite3

from scripts.logging.custom_logging import setup_logger
from scripts.util.load_env import load_github_env_vars

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/db"))
os.makedirs(OUTPUT_DIR, exist_ok=True)


def create_tables(cur):
    print("Creating database tables (issues, pull_requests, labels, issue_labels)...")

    common_schema = """
        id INTEGER PRIMARY KEY,
        number INTEGER,
        title TEXT,
        state TEXT,
        created_at TEXT,
        updated_at TEXT,
        closed_at TEXT,
        user_login TEXT
    """

    cur.execute(f"""
        CREATE TABLE IF NOT EXISTS issues(
            {common_schema}
        )
    """)
    cur.execute(f"""
        CREATE TABLE IF NOT EXISTS pull_requests(
            {common_schema},
            is_draft BOOLEAN
        )
    """)
    cur.execute("""
        CREATE TABLE IF NOT EXISTS labels(
            id INTEGER PRIMARY KEY,
            name TEXT,
            color TEXT,
            description TEXT
        )
    """)
    cur.execute("""
        CREATE TABLE IF NOT EXISTS issue_labels(
            issue_id INTEGER,
            label_id INTEGER,
            PRIMARY KEY (issue_id, label_id)
        )
    """)
    print("Database tables created successfully.")


def write_issues_to_sqlite(issues, output_dir, repo_owner, repo_name):
    db_filename = f"{repo_owner}_{repo_name}.db"
    db_path = os.path.join(output_dir, db_filename)

    if os.path.exists(db_path):
        print(f"Deleting database at {db_path}...")
        os.remove(db_path)
    print(f"Setting up SQLite database at {db_path}...")

    conn = sqlite3.connect(db_path)
    cur = conn.cursor()

    create_tables(cur)

    issue_rows = []
    pr_rows = []
    label_map = {}
    issue_label_rows = []

    for issue in issues:
        issue_id = issue.get("id")
        number = issue.get("number")
        title = issue.get("title")
        state = issue.get("state")
        created_at = issue.get("created_at")
        updated_at = issue.get("updated_at")
        closed_at = issue.get("closed_at")
        user_login = issue.get("user", {}).get("login") if issue.get("user") else None

        row = (issue_id, number, title, state, created_at, updated_at, closed_at, user_login)

        # GitHub API is funny, it returns issues and pull requests in the same endpoint.
        if "pull_request" in issue:
            is_draft = issue.get("draft", False)
            pr_row = row + (is_draft,)
            pr_rows.append(pr_row)
        else:
            issue_rows.append(row)

        labels = issue.get("labels", [])
        for label in labels:
            lbl_id = label.get("id")
            name = label.get("name")
            color = label.get("color")
            desc = label.get("description")
            if lbl_id is not None and lbl_id not in label_map:
                label_map[lbl_id] = (lbl_id, name, color, desc)
            if lbl_id is not None:
                issue_label_rows.append((issue_id, lbl_id))

    print("Inserting issues into database...")
    if issue_rows:
        cur.executemany("""
            INSERT INTO issues(id, number, title, state, created_at, updated_at, closed_at, user_login)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        """, issue_rows)
    print(f"Inserted {len(issue_rows)} issues into the database.")

    print("Inserting pull requests into database...")
    if pr_rows:
        cur.executemany("""
            INSERT INTO pull_requests(id, number, title, state, created_at, updated_at, closed_at, user_login, is_draft)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        """, pr_rows)
    print(f"Inserted {len(pr_rows)} pull requests into the database.")

    print("Inserting labels into database...")
    label_rows = list(label_map.values())
    if label_rows:
        cur.executemany("""
            INSERT INTO labels(id, name, color, description)
            VALUES (?, ?, ?, ?)
        """, label_rows)
    print(f"Inserted {len(label_rows)} labels into the database.")

    print("Inserting issue-label relationships into database...")
    unique_issue_label_rows = list({(iid, lid) for (iid, lid) in issue_label_rows})
    if unique_issue_label_rows:
        cur.executemany("""
            INSERT INTO issue_labels(issue_id, label_id)
            VALUES (?, ?)
        """, unique_issue_label_rows)
    print(f"Inserted {len(unique_issue_label_rows)} issue-label records into the database.")

    conn.commit()
    conn.close()
    print(f"Database population complete. SQLite DB saved at {db_path}.")

    return db_path


def read_json_file(filepath):
    try:
        with open(filepath, 'r', encoding='utf-8') as f:
            data = json.load(f)
            return data
    except FileNotFoundError:
        print(f"Error: File not found - {filepath}")
    except json.JSONDecodeError as e:
        print(f"Error: Failed to decode JSON - {e}")
    except Exception as e:
        print(f"Unexpected error: {e}")

    return None


# Regenerate the database if it already exists.
def main():
    setup_logger()

    parser = argparse.ArgumentParser(description="Load GitHub issues from a JSON archive into SQLite.")
    parser.add_argument("--input", dest="input", required=True, help="Path to the GitHub issues JSON archive")
    parser.add_argument(
        "--env-file",
        type=str,
        help="Path to the .env file to load environment variables from",
    )
    args = parser.parse_args()

    try:
        env = load_github_env_vars(args.env_file)
    except ValueError as e:
        print(f"Error loading environment variables: {e}")
        return 1

    issues = read_json_file(args.input)
    if not issues:
        print("No data found. Exiting.")
        return 1

    write_issues_to_sqlite(
        issues=issues,
        output_dir=OUTPUT_DIR,
        repo_owner=env['REPO_OWNER'],
        repo_name=env['REPO_NAME'],
    )


if __name__ == "__main__":
    main()
