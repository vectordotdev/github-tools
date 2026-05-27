use crate::commands::fetch_issues::parse_since;
use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;

const GRAPHQL_URL: &str = "https://api.github.com/graphql";
const PR_PAGE_SIZE: u32 = 10;

const MERGED_PRS_QUERY: &str = r#"
query($owner: String!, $name: String!, $first: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(first: $first, after: $after, states: [MERGED], orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo {
        endCursor
        hasNextPage
      }
      nodes {
        number
        mergedAt
        reviewThreads(first: 30) {
          nodes {
            comments(first: 10) {
              nodes {
                url
                author { login }
                reactions(first: 20) {
                  nodes {
                    content
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}
"#;

struct Stats {
    prs_scanned: u64,
    total: u64,
    liked: u64,
    disliked: u64,
    no_reaction: u64,
}

/// Omit `bot_login` to run in discovery mode: lists all review comment authors.
pub fn run(config: &Config, bot_login: Option<&str>, since: Option<&str>) -> Result<()> {
    let client = Client::new();
    let since_ts = since.map(parse_since).transpose()?;

    let discovery = bot_login.is_none();
    let mut authors: HashMap<String, u64> = HashMap::new();
    let mut stats = Stats { prs_scanned: 0, total: 0, liked: 0, disliked: 0, no_reaction: 0 };
    let mut rows: Vec<(String, &'static str)> = Vec::new();
    let mut after: Option<String> = None;
    let mut page = 1;

    loop {
        println!("Fetching merged PR page {page} (batch: {PR_PAGE_SIZE})...");

        let body = json!({
            "query": MERGED_PRS_QUERY,
            "variables": {
                "owner": config.repo_owner,
                "name": config.repo_name,
                "first": PR_PAGE_SIZE,
                "after": after,
            }
        });

        let response = client
            .post(GRAPHQL_URL)
            .header("Authorization", format!("Bearer {}", config.github_token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "github-tools")
            .json(&body)
            .send()
            .context("Failed to send GraphQL request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            anyhow::bail!("GraphQL request failed ({status}): {text}");
        }

        let result: Value = response.json().context("Failed to parse GraphQL response")?;

        if let Some(errors) = result.get("errors") {
            anyhow::bail!("GraphQL errors: {errors}");
        }

        let connection = &result["data"]["repository"]["pullRequests"];
        let nodes = connection["nodes"].as_array().context("Missing nodes")?;
        let has_next = connection["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
        let end_cursor = connection["pageInfo"]["endCursor"].as_str().map(|s| s.to_string());

        let mut hit_boundary = false;
        for pr in nodes {
            let merged_at = pr["mergedAt"].as_str().unwrap_or("");
            if let Some(ref ts) = since_ts {
                if merged_at < ts.as_str() {
                    hit_boundary = true;
                    continue;
                }
            }
            stats.prs_scanned += 1;

            let threads = pr["reviewThreads"]["nodes"].as_array().cloned().unwrap_or_default();
            for thread in &threads {
                let comments = thread["comments"]["nodes"].as_array().cloned().unwrap_or_default();
                for comment in &comments {
                    let author = comment["author"]["login"].as_str().unwrap_or("");

                    if discovery {
                        *authors.entry(author.to_string()).or_default() += 1;
                        continue;
                    }

                    if Some(author) != bot_login {
                        continue;
                    }

                    stats.total += 1;
                    let url = comment["url"].as_str().unwrap_or("").to_string();
                    let reactions = comment["reactions"]["nodes"].as_array().cloned().unwrap_or_default();
                    let has_up = reactions.iter().any(|r| r["content"].as_str() == Some("THUMBS_UP"));
                    let has_down = reactions.iter().any(|r| r["content"].as_str() == Some("THUMBS_DOWN"));

                    let reaction = if has_up {
                        stats.liked += 1;
                        "liked"
                    } else if has_down {
                        stats.disliked += 1;
                        "disliked"
                    } else {
                        stats.no_reaction += 1;
                        "no reaction"
                    };
                    rows.push((url, reaction));
                }
            }
        }

        println!("  PRs scanned: {} | bot comments found: {}", stats.prs_scanned, stats.total);

        if hit_boundary || !has_next {
            break;
        }
        after = end_cursor;
        page += 1;
    }

    println!();

    if discovery {
        println!("=== Review Comment Authors ({}/{}) ===", config.repo_owner, config.repo_name);
        if let Some(ref ts) = since_ts {
            println!("Since : {ts}");
        }
        println!("PRs scanned : {}", stats.prs_scanned);
        println!();
        let mut sorted: Vec<_> = authors.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        for (login, count) in &sorted {
            println!("  {count:>6}  {login}");
        }
        return Ok(());
    }

    let login = bot_login.unwrap();
    println!("=== Automated Review Stats ({}/{}) ===", config.repo_owner, config.repo_name);
    println!("Bot login : {login}");
    if let Some(ref ts) = since_ts {
        println!("Since     : {ts}");
    }
    println!("PRs scanned        : {}", stats.prs_scanned);
    println!("Total bot comments : {}", stats.total);
    println!("  Liked    (THUMBS_UP)   : {}", stats.liked);
    println!("  Disliked (THUMBS_DOWN) : {}", stats.disliked);
    println!("  No reaction            : {}", stats.no_reaction);
    if stats.total > 0 {
        println!();
        println!("  Like rate    : {:.1}%", stats.liked as f64 / stats.total as f64 * 100.0);
        println!("  Dislike rate : {:.1}%", stats.disliked as f64 / stats.total as f64 * 100.0);
    }

    if !rows.is_empty() {
        let out_dir = std::path::Path::new("out/automated-review-stats");
        fs::create_dir_all(out_dir)?;
        let csv_path = out_dir.join(format!("{}_{}.csv", config.repo_owner, config.repo_name));
        rows.sort_by_key(|(_, reaction)| match *reaction {
            "no reaction" => 0,
            "disliked" => 1,
            _ => 2,
        });
        let mut csv = String::from("url, reaction\n");
        for (url, reaction) in &rows {
            csv.push_str(&format!("{url}, {reaction}\n"));
        }
        fs::write(&csv_path, csv)?;
        println!();
        println!("Full table written to {}", csv_path.display());
    }

    if stats.total > 0 {
        update_trends(config, login, since_ts.as_deref(), &stats)?;
    }

    Ok(())
}

fn update_trends(config: &Config, bot_login: &str, since_ts: Option<&str>, stats: &Stats) -> Result<()> {
    let trends_path = std::path::Path::new("trends").join(format!("{}.md", config.repo_name));
    if !trends_path.exists() {
        println!("Trends file not found at {}, skipping.", trends_path.display());
        return Ok(());
    }

    let reacted = stats.liked + stats.disliked;
    let since_label = since_ts.unwrap_or("all time");

    let new_content = format!(
        "\n**All comments** ({} merged PRs, bot: `{bot_login}`, since: {since_label})\n\
        \n\
        | Reaction | Count | Share |\n\
        |----------|------:|------:|\n\
        | Liked 👍 | {liked} | {liked_pct:.1}% |\n\
        | Disliked 👎 | {disliked} | {disliked_pct:.1}% |\n\
        | No reaction | {no_reaction} | {no_reaction_pct:.1}% |\n\
        | **Total** | **{total}** | |\n\
        \n\
        **Reacted comments only** (excludes no reaction)\n\
        \n\
        | Reaction | Count | Share |\n\
        |----------|------:|------:|\n\
        | Liked 👍 | {liked} | {liked_reacted_pct:.1}% |\n\
        | Disliked 👎 | {disliked} | {disliked_reacted_pct:.1}% |\n\
        | **Total** | **{reacted}** | |\n",
        stats.prs_scanned,
        liked = stats.liked,
        disliked = stats.disliked,
        no_reaction = stats.no_reaction,
        total = stats.total,
        liked_pct = stats.liked as f64 / stats.total as f64 * 100.0,
        disliked_pct = stats.disliked as f64 / stats.total as f64 * 100.0,
        no_reaction_pct = stats.no_reaction as f64 / stats.total as f64 * 100.0,
        liked_reacted_pct = stats.liked as f64 / reacted as f64 * 100.0,
        disliked_reacted_pct = stats.disliked as f64 / reacted as f64 * 100.0,
    );

    const START: &str = "<!-- AUTO:automated-review-stats:start -->";
    const END: &str = "<!-- AUTO:automated-review-stats:end -->";

    let existing = fs::read_to_string(&trends_path)?;

    let updated = match (existing.find(START), existing.find(END)) {
        (Some(start_pos), Some(end_pos)) => {
            let before = &existing[..start_pos + START.len()];
            let after = &existing[end_pos..];
            format!("{before}{new_content}{after}")
        }
        _ => format!("{existing}\n## AI-Assisted Code Review\n\n{START}{new_content}{END}\n"),
    };

    fs::write(&trends_path, updated)?;
    println!("Trends updated at {}", trends_path.display());

    Ok(())
}
