use crate::commands::fetch_discussions::Discussion;
use crate::config::Config;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// Load issues/PRs from a single JSON file or a directory of JSON files.
pub fn run(input: &str, config: &Config) -> Result<()> {
    let input_path = Path::new(input);
    let items = if input_path.is_dir() {
        load_json_dir(input_path)?
    } else {
        load_json_file(input_path)?
    };

    println!("Loaded {} items from {input}", items.len());

    let out_dir = Path::new("out/db");
    fs::create_dir_all(out_dir)?;
    let db_path = out_dir.join(format!("{}_{}.db", config.repo_owner, config.repo_name));

    if db_path.exists() {
        println!("Deleting existing database at {}...", db_path.display());
        fs::remove_file(&db_path)?;
    }
    println!("Setting up SQLite database at {}...", db_path.display());

    let conn = Connection::open(&db_path)?;
    create_tables(&conn)?;
    insert_data(&conn, &items)?;

    println!("Database saved to '{}'", db_path.display());
    Ok(())
}

fn load_json_file(path: &Path) -> Result<Vec<Value>> {
    let json = fs::read_to_string(path)
        .with_context(|| format!("Failed to read input file: {}", path.display()))?;
    serde_json::from_str(&json).context("Failed to parse issues JSON")
}

fn load_json_dir(dir: &Path) -> Result<Vec<Value>> {
    let mut all_items = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        let items = load_json_file(&path)?;
        println!("  Loaded {} items from {}", items.len(), path.display());
        all_items.extend(items);
    }
    Ok(all_items)
}

fn create_tables(conn: &Connection) -> Result<()> {
    println!("Creating database tables...");
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS issues (
            id          INTEGER PRIMARY KEY,
            number      INTEGER,
            title       TEXT,
            state       TEXT,
            created_at  TEXT,
            updated_at  TEXT,
            closed_at   TEXT,
            user_login  TEXT,
            issue_type  TEXT
        );
        CREATE TABLE IF NOT EXISTS pull_requests (
            id          INTEGER PRIMARY KEY,
            number      INTEGER,
            title       TEXT,
            state       TEXT,
            created_at  TEXT,
            updated_at  TEXT,
            closed_at   TEXT,
            user_login  TEXT,
            is_draft    BOOLEAN
        );
        CREATE TABLE IF NOT EXISTS labels (
            id          INTEGER PRIMARY KEY,
            name        TEXT,
            color       TEXT,
            description TEXT
        );
        CREATE TABLE IF NOT EXISTS issue_labels (
            issue_id    INTEGER,
            label_id    INTEGER,
            PRIMARY KEY (issue_id, label_id)
        );
        CREATE TABLE IF NOT EXISTS discussions (
            number        INTEGER PRIMARY KEY,
            title         TEXT,
            category      TEXT,
            created_at    TEXT,
            updated_at    TEXT,
            closed_at     TEXT,
            closed        BOOLEAN,
            state_reason  TEXT,
            is_answered   BOOLEAN,
            user_login    TEXT,
            comment_count INTEGER,
            upvote_count  INTEGER
        );
    ",
    )?;
    println!("Database tables created successfully.");
    Ok(())
}

type IssueRow = (i64, i64, String, String, String, String, Option<String>, Option<String>, Option<String>);
type PrRow = (i64, i64, String, String, String, String, Option<String>, Option<String>, bool);

fn insert_data(conn: &Connection, items: &[Value]) -> Result<()> {
    let mut issue_rows: Vec<IssueRow> = Vec::new();
    let mut pr_rows: Vec<PrRow> = Vec::new();
    let mut label_map: HashMap<i64, (i64, String, String, Option<String>)> = HashMap::new();
    let mut issue_label_set: HashSet<(i64, i64)> = HashSet::new();

    for item in items {
        let id = item["id"].as_i64().context("missing id")?;
        let number = item["number"].as_i64().context("missing number")?;
        let title = item["title"].as_str().unwrap_or("").to_string();
        let state = item["state"].as_str().unwrap_or("").to_string();
        let created_at = item["created_at"].as_str().unwrap_or("").to_string();
        let updated_at = item["updated_at"].as_str().unwrap_or("").to_string();
        let closed_at = item["closed_at"].as_str().map(|s| s.to_string());
        let user_login = item["user"]["login"].as_str().map(|s| s.to_string());
        let issue_type = item["issue_type"].as_str().map(|s| s.to_string());

        if item.get("pull_request").is_some() {
            let is_draft = item["draft"].as_bool().unwrap_or(false);
            pr_rows.push((
                id, number, title, state, created_at, updated_at, closed_at, user_login, is_draft,
            ));
        } else {
            issue_rows.push((
                id, number, title, state, created_at, updated_at, closed_at, user_login, issue_type,
            ));
        }

        if let Some(labels) = item["labels"].as_array() {
            for label in labels {
                if let Some(lbl_id) = label["id"].as_i64() {
                    label_map.entry(lbl_id).or_insert_with(|| {
                        let name = label["name"].as_str().unwrap_or("").to_string();
                        let color = label["color"].as_str().unwrap_or("").to_string();
                        let desc = label["description"].as_str().map(|s| s.to_string());
                        (lbl_id, name, color, desc)
                    });
                    issue_label_set.insert((id, lbl_id));
                }
            }
        }
    }

    println!("Inserting issues into database...");
    {
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO issues(id, number, title, state, created_at, updated_at, closed_at, user_login, issue_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
        )?;
        for (id, number, title, state, created_at, updated_at, closed_at, user_login, issue_type) in &issue_rows
        {
            stmt.execute(rusqlite::params![
                id, number, title, state, created_at, updated_at, closed_at, user_login, issue_type
            ])?;
        }
    }
    println!("Inserted {} issues.", issue_rows.len());

    println!("Inserting pull requests into database...");
    {
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO pull_requests(id, number, title, state, created_at, updated_at, closed_at, user_login, is_draft)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
        )?;
        for (id, number, title, state, created_at, updated_at, closed_at, user_login, is_draft) in
            &pr_rows
        {
            stmt.execute(rusqlite::params![
                id, number, title, state, created_at, updated_at, closed_at, user_login, is_draft
            ])?;
        }
    }
    println!("Inserted {} pull requests.", pr_rows.len());

    println!("Inserting labels into database...");
    {
        let mut stmt = conn
            .prepare("INSERT OR REPLACE INTO labels(id, name, color, description) VALUES (?1, ?2, ?3, ?4)")?;
        for (id, name, color, desc) in label_map.values() {
            stmt.execute(rusqlite::params![id, name, color, desc])?;
        }
    }
    println!("Inserted {} labels.", label_map.len());

    println!("Inserting issue-label relationships into database...");
    {
        let mut stmt =
            conn.prepare("INSERT OR REPLACE INTO issue_labels(issue_id, label_id) VALUES (?1, ?2)")?;
        for (issue_id, label_id) in &issue_label_set {
            stmt.execute(rusqlite::params![issue_id, label_id])?;
        }
    }
    println!("Inserted {} issue-label records.", issue_label_set.len());

    Ok(())
}

/// Load discussions from a single JSON file or a directory of JSON files.
pub fn load_discussions_from_path(conn: &Connection, input: &str) -> Result<()> {
    let path = Path::new(input);
    let discussions: Vec<Discussion> = if path.is_dir() {
        let mut all = Vec::new();
        let mut entries: Vec<_> = fs::read_dir(path)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let json = fs::read_to_string(entry.path())?;
            let items: Vec<Discussion> = serde_json::from_str(&json)?;
            println!("  Loaded {} discussions from {}", items.len(), entry.path().display());
            all.extend(items);
        }
        all
    } else {
        let json = fs::read_to_string(path)
            .with_context(|| format!("Failed to read: {}", path.display()))?;
        serde_json::from_str(&json)?
    };
    println!("Loaded {} discussions from {input}", discussions.len());
    load_discussions(conn, &discussions)
}

pub fn load_discussions(conn: &Connection, discussions: &[Discussion]) -> Result<()> {
    println!("Inserting discussions into database...");
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO discussions(number, title, category, created_at, updated_at, closed_at, closed, state_reason, is_answered, user_login, comment_count, upvote_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
    )?;
    for d in discussions {
        stmt.execute(rusqlite::params![
            d.number,
            d.title,
            d.category.name,
            d.created_at,
            d.updated_at,
            d.closed_at,
            d.closed,
            d.state_reason,
            d.is_answered,
            d.author.as_ref().map(|a| a.login.as_str()),
            d.comments.total_count,
            d.upvote_count,
        ])?;
    }
    println!("Inserted {} discussions.", discussions.len());
    Ok(())
}
