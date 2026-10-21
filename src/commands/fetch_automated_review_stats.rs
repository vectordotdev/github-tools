//! Persist review comments like the other year-bucketed GitHub snapshots.
//! Discovery is incremental; reactions on every saved comment are reconciled each run.
use super::sorted_json;
use crate::commands::fetch_issues::parse_since;
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const PRS: &str = r#"
query($owner:String!, $name:String!, $after:String) {
 repository(owner:$owner,name:$name) {
  pullRequests(first:100,after:$after,states:[MERGED],orderBy:{field:UPDATED_AT,direction:DESC}) {
   pageInfo {hasNextPage endCursor}
   nodes {number updatedAt mergedAt}
  }
 }
}"#;
const THREADS: &str = r#"
query($owner:String!, $name:String!, $number:Int!, $after:String) {
 repository(owner:$owner,name:$name) { pullRequest(number:$number) {
  reviewThreads(first:100,after:$after) {
   pageInfo {hasNextPage endCursor}
   nodes {id comments(first:100) {
    pageInfo {hasNextPage endCursor}
    nodes {COMMENT_FIELDS}
   }}
  }
 }}
}"#;
const COMMENTS: &str = r#"
query($id:ID!, $after:String) {
 node(id:$id) { ... on PullRequestReviewThread { comments(first:100,after:$after) {
  pageInfo {hasNextPage endCursor}
  nodes {COMMENT_FIELDS}
 }}}
}"#;
const COMMENT_FIELDS: &str = "id author {login} up:reactions(content:THUMBS_UP) {totalCount} down:reactions(content:THUMBS_DOWN) {totalCount}";
const REFRESH: &str = r#"
query($ids:[ID!]!) {
 nodes(ids:$ids) { ... on PullRequestReviewComment {COMMENT_FIELDS} }
}"#;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct Comment {
    id: String,
    pr: u64,
    merged_at: String,
    thumbs_up: u64,
    thumbs_down: u64,
}

/// Minimal per-comment state required to reconcile reactions by node ID.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReviewComment {
    id: String,
    thumbs_up: u64,
    thumbs_down: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct PullRequest {
    pr: u64,
    merged_at: String,
    comments: Vec<ReviewComment>,
}

#[derive(Debug, Serialize)]
struct PrSummary {
    pr: u64,
    url: String,
    merged_at: String,
    total: usize,
    liked: usize,
    disliked: usize,
    no_signal: usize,
    mixed: usize,
    thumbs_up: u64,
    thumbs_down: u64,
}

fn group_by_pr(records: &BTreeMap<String, Comment>) -> Result<BTreeMap<u64, PullRequest>> {
    let mut prs: BTreeMap<u64, PullRequest> = BTreeMap::new();
    for comment in records.values() {
        let pr = prs.entry(comment.pr).or_insert_with(|| PullRequest {
            pr: comment.pr,
            merged_at: comment.merged_at.clone(),
            comments: Vec::new(),
        });
        ensure!(
            pr.merged_at == comment.merged_at,
            "Conflicting merge dates for PR {}",
            pr.pr
        );
        pr.comments.push(ReviewComment {
            id: comment.id.clone(),
            thumbs_up: comment.thumbs_up,
            thumbs_down: comment.thumbs_down,
        });
    }
    Ok(prs)
}

fn pr_summaries(repo: &str, records: &BTreeMap<String, Comment>) -> Result<Vec<PrSummary>> {
    Ok(group_by_pr(records)?
        .into_values()
        .map(|pr| {
            let liked = pr
                .comments
                .iter()
                .filter(|c| c.thumbs_up > 0 && c.thumbs_down == 0)
                .count();
            let disliked = pr
                .comments
                .iter()
                .filter(|c| c.thumbs_down > 0 && c.thumbs_up == 0)
                .count();
            PrSummary {
                pr: pr.pr,
                url: format!("https://github.com/{repo}/pull/{}", pr.pr),
                merged_at: pr.merged_at,
                total: pr.comments.len(),
                liked,
                disliked,
                no_signal: pr.comments.len() - liked - disliked,
                mixed: pr
                    .comments
                    .iter()
                    .filter(|c| c.thumbs_up > 0 && c.thumbs_down > 0)
                    .count(),
                thumbs_up: pr.comments.iter().map(|c| c.thumbs_up).sum(),
                thumbs_down: pr.comments.iter().map(|c| c.thumbs_down).sum(),
            }
        })
        .collect())
}

#[derive(Debug, Deserialize, Serialize)]
struct State {
    schema_version: u32,
    repo: String,
    bot_login: String,
    since: String,
    // Start of the successful discovery pass, never its end, avoids skipping races.
    discovery_through: String,
    collected_at: String,
    prs_scanned: u64,
}

fn root(config: &Config) -> PathBuf {
    Path::new("data")
        .join(format!("{}_{}", config.org, config.repo))
        .join("automated-review-stats")
}

fn query(client: &Client, config: &Config, query: &str, variables: Value) -> Result<Value> {
    let response = client.post("https://api.github.com/graphql")
        .bearer_auth(&config.github_token)
        .header("User-Agent", "github-tools")
        .json(&json!({"query":query.replace("COMMENT_FIELDS", COMMENT_FIELDS), "variables":variables}))
        .send()?.error_for_status()?;
    let value: Value = response.json()?;
    if let Some(errors) = value.get("errors") {
        bail!("GitHub GraphQL errors: {errors}");
    }
    ensure!(value["data"].is_object(), "Missing GraphQL data");
    Ok(value["data"].clone())
}

fn nodes(connection: &Value) -> Result<&Vec<Value>> {
    connection["nodes"]
        .as_array()
        .context("Missing connection nodes")
}

fn next(connection: &Value) -> Result<Option<String>> {
    let has_next = connection["pageInfo"]["hasNextPage"]
        .as_bool()
        .context("Missing pageInfo")?;
    if has_next {
        Ok(Some(string(&connection["pageInfo"], "endCursor")?))
    } else {
        Ok(None)
    }
}

fn string(value: &Value, key: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("Missing {key}"))
}

fn timestamp(value: &str) -> Result<String> {
    Ok(DateTime::parse_from_rfc3339(value)?
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn comment(value: &Value, pr: u64, merged_at: &str) -> Result<Comment> {
    Ok(Comment {
        id: string(value, "id")?,
        pr,
        merged_at: timestamp(merged_at)?,
        thumbs_up: value["up"]["totalCount"]
            .as_u64()
            .context("Missing thumbs-up count")?,
        thumbs_down: value["down"]["totalCount"]
            .as_u64()
            .context("Missing thumbs-down count")?,
    })
}

fn read_records(dir: &Path) -> Result<BTreeMap<String, Comment>> {
    let mut records = BTreeMap::new();
    let mut seen_prs = std::collections::BTreeSet::new();
    if !dir.exists() {
        return Ok(records);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if path.extension().and_then(|s| s.to_str()) != Some("json")
            || name.len() != 4
            || !name.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let items: Vec<PullRequest> = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("Reading {}", path.display()))?;
        for pr in items {
            ensure!(
                timestamp(&pr.merged_at)?.starts_with(name),
                "PR in wrong merge-year bucket"
            );
            ensure!(seen_prs.insert(pr.pr), "Duplicate PR {}", pr.pr);
            for item in pr.comments {
                ensure!(
                    !records.contains_key(&item.id),
                    "Duplicate comment ID {}",
                    item.id
                );
                records.insert(
                    item.id.clone(),
                    Comment {
                        id: item.id,
                        pr: pr.pr,
                        merged_at: pr.merged_at.clone(),
                        thumbs_up: item.thumbs_up,
                        thumbs_down: item.thumbs_down,
                    },
                );
            }
        }
    }
    Ok(records)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let text = format!("{}\n", sorted_json(value)?);
    if fs::read_to_string(path).ok().as_deref() == Some(&text) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().context("Missing parent")?)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, text)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn save(dir: &Path, records: &BTreeMap<String, Comment>, state: &State) -> Result<()> {
    let mut years: BTreeMap<String, Vec<PullRequest>> = BTreeMap::new();
    // Retain empty buckets when comments were deleted; no stale records remain.
    if dir.exists() {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let year = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if path.extension().and_then(|s| s.to_str()) == Some("json")
                && year.len() == 4
                && year.bytes().all(|b| b.is_ascii_digit())
            {
                years.entry(year.to_owned()).or_default();
            }
        }
    }
    for item in group_by_pr(records)?.into_values() {
        years
            .entry(timestamp(&item.merged_at)?[..4].to_owned())
            .or_default()
            .push(item);
    }
    for (year, items) in years {
        write_json(&dir.join(format!("{year}.json")), &items)?;
    }
    // Advance only after all requests and year writes succeed. Failed runs can safely retry.
    write_json(&dir.join("state.json"), state)
}

fn cutoff(requested: Option<&str>, state: Option<&State>) -> Result<String> {
    let since = requested
        .map(parse_since)
        .transpose()?
        .map(|s| timestamp(&s))
        .transpose()?
        .unwrap_or_else(|| {
            state
                .map(|s| s.since.clone())
                .unwrap_or("1970-01-01T00:00:00Z".into())
        });
    if let Some(state) = state {
        ensure!(
            since <= state.since,
            "--since cannot narrow saved coverage ({}); omit it to reuse coverage, or choose an earlier date to backfill",
            state.since
        );
    }
    Ok(since)
}

fn discovery_boundary(since: &str, state: Option<&State>, full_refresh: bool) -> Result<String> {
    if let Some(state) = state.filter(|s| !full_refresh && since == s.since) {
        // A one-day overlap tolerates timestamp precision and delayed visibility.
        let boundary = DateTime::parse_from_rfc3339(&state.discovery_through)? - Duration::days(1);
        Ok(boundary
            .to_rfc3339_opts(SecondsFormat::Secs, true)
            .max(since.to_owned()))
    } else {
        Ok(since.to_owned())
    }
}

/// Without a bot, discover authors without persisting anything.
pub fn run(
    config: &Config,
    bot_login: Option<&str>,
    since: Option<&str>,
    full_refresh: bool,
) -> Result<()> {
    if let Some(bot) = bot_login {
        ensure!(
            !bot.is_empty()
                && bot
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'[' || b == b']'),
            "Invalid bot login"
        );
    }
    let dir = bot_login.map(|bot| root(config).join(bot));
    let state: Option<State> = match dir.as_ref().map(|d| d.join("state.json")) {
        Some(path) if path.exists() => Some(serde_json::from_slice(&fs::read(path)?)?),
        _ => None,
    };
    if let Some(state) = &state {
        ensure!(
            state.schema_version == 2
                && state.repo == format!("{}/{}", config.org, config.repo)
                && Some(state.bot_login.as_str()) == bot_login,
            "Saved state identity/version mismatch"
        );
    }
    let since = cutoff(since, state.as_ref())?;
    let boundary = discovery_boundary(&since, state.as_ref(), full_refresh)?;
    let mut records = match &dir {
        Some(dir) => read_records(dir)?,
        None => BTreeMap::new(),
    };
    ensure!(
        records.is_empty() || state.is_some(),
        "Saved comments require state.json; refusing to guess coverage"
    );
    let started = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let mut authors: BTreeMap<String, u64> = BTreeMap::new();
    let mut prs_scanned = 0;
    let mut after: Option<String> = None;
    println!("Discovering PRs updated since {boundary}; merged coverage begins {since}");
    loop {
        let data = query(
            &client,
            config,
            PRS,
            json!({"owner":config.org,"name":config.repo,"after":after}),
        )?;
        let connection = &data["repository"]["pullRequests"];
        let mut hit_boundary = false;
        for pr in nodes(connection)? {
            if timestamp(&string(pr, "updatedAt")?)? < boundary {
                hit_boundary = true;
                continue;
            }
            let merged_at = timestamp(&string(pr, "mergedAt")?)?;
            if merged_at < since {
                continue;
            }
            let number = pr["number"].as_u64().context("Missing PR number")?;
            let fresh = fetch_comments(
                config,
                number,
                &merged_at,
                bot_login,
                &records,
                &mut authors,
                |q, variables| query(&client, config, q, variables),
            )?;
            // Replace the scanned PR's records, including removal of deleted comments.
            records.retain(|_, c| c.pr != number);
            records.extend(fresh);
            prs_scanned += 1;
        }
        println!(
            "Scanned {prs_scanned} PRs; {} saved bot comments",
            records.len()
        );
        after = next(connection)?;
        if hit_boundary || after.is_none() {
            break;
        }
    }
    let Some(bot) = bot_login else {
        let mut authors: Vec<_> = authors.into_iter().collect();
        authors.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (author, count) in authors {
            println!("{count:>6} {author}");
        }
        return Ok(());
    };
    // Reactions may change without changing the parent PR's updatedAt.
    // Bounded nodes queries avoid enumerating individual reactions or all old PRs.
    let ids: Vec<_> = records.keys().cloned().collect();
    for batch in ids.chunks(100) {
        let data = query(&client, config, REFRESH, json!({"ids":batch}))?;
        apply_refresh(&mut records, batch, &data["nodes"], bot)?;
    }
    let state = State {
        schema_version: 2,
        repo: format!("{}/{}", config.org, config.repo),
        bot_login: bot.into(),
        since,
        discovery_through: started,
        collected_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        prs_scanned,
    };
    save(&dir.context("Missing bot directory")?, &records, &state)?;
    generate_summaries(config)?;
    println!(
        "Saved {} comments for {bot}; reactions reconciled through {}",
        records.len(),
        state.collected_at
    );
    Ok(())
}

fn fetch_comments(
    config: &Config,
    number: u64,
    merged_at: &str,
    bot: Option<&str>,
    saved: &BTreeMap<String, Comment>,
    authors: &mut BTreeMap<String, u64>,
    mut request: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<BTreeMap<String, Comment>> {
    let mut fresh = BTreeMap::new();
    let mut thread_after: Option<String> = None;
    loop {
        let data = request(
            THREADS,
            json!({"owner":config.org,"name":config.repo,"number":number,"after":thread_after}),
        )?;
        let threads = &data["repository"]["pullRequest"]["reviewThreads"];
        for thread in nodes(threads)? {
            let mut comments = thread["comments"].clone();
            loop {
                for value in nodes(&comments)? {
                    let author = value["author"]["login"].as_str();
                    *authors
                        .entry(author.unwrap_or("[deleted]").to_owned())
                        .or_default() += 1;
                    let previously_attributed = author.is_none()
                        && value["id"]
                            .as_str()
                            .is_some_and(|id| saved.contains_key(id));
                    if bot.is_some() && (author == bot || previously_attributed) {
                        let item = comment(value, number, merged_at)?;
                        fresh.insert(item.id.clone(), item);
                    }
                }
                let Some(cursor) = next(&comments)? else {
                    break;
                };
                let data = request(COMMENTS, json!({"id":string(thread,"id")?,"after":cursor}))?;
                comments = data["node"]["comments"].clone();
            }
        }
        thread_after = next(threads)?;
        if thread_after.is_none() {
            break;
        }
    }
    Ok(fresh)
}

fn apply_refresh(
    records: &mut BTreeMap<String, Comment>,
    ids: &[String],
    values: &Value,
    bot: &str,
) -> Result<()> {
    let values = values.as_array().context("Missing refreshed nodes")?;
    ensure!(values.len() == ids.len(), "Incomplete reaction refresh");
    for (id, value) in ids.iter().zip(values) {
        if value.is_null() {
            records.remove(id);
            continue;
        }
        ensure!(
            value["id"].as_str() == Some(id),
            "Reaction refresh ID mismatch"
        );
        // Deleted users have null authors. Preserve attribution already observed.
        if let Some(author) = value["author"]["login"].as_str() {
            ensure!(
                author == bot,
                "Saved comment author changed; run --full-refresh"
            );
        }
        let previous = &records[id];
        let fresh = comment(value, previous.pr, &previous.merged_at)?;
        records.insert(id.clone(), fresh);
    }
    Ok(())
}

fn summary(state: &State, records: &BTreeMap<String, Comment>) -> Value {
    let liked = records
        .values()
        .filter(|c| c.thumbs_up > 0 && c.thumbs_down == 0)
        .count();
    let disliked = records
        .values()
        .filter(|c| c.thumbs_down > 0 && c.thumbs_up == 0)
        .count();
    let mixed = records
        .values()
        .filter(|c| c.thumbs_down > 0 && c.thumbs_up > 0)
        .count();
    json!({"bot_login":state.bot_login,"since":state.since,"collected_at":state.collected_at,
        "prs_scanned":state.prs_scanned,"total":records.len(),"liked":liked,"disliked":disliked,
        "no_signal":records.len()-liked-disliked,"mixed":mixed})
}

/// Rebuild disposable summaries from tracked data; usable offline by generate-all.
pub fn generate_summaries(config: &Config) -> Result<()> {
    let root = root(config);
    if !root.exists() {
        return Ok(());
    }
    let mut summaries = BTreeMap::new();
    for entry in fs::read_dir(root)? {
        let dir = entry?.path();
        if !dir.is_dir() {
            continue;
        }
        let state: State = serde_json::from_slice(&fs::read(dir.join("state.json"))?)?;
        ensure!(
            state.schema_version == 2
                && state.repo == format!("{}/{}", config.org, config.repo)
                && dir.file_name().and_then(|n| n.to_str()) == Some(state.bot_login.as_str()),
            "Saved state identity/version mismatch"
        );
        let records = read_records(&dir)?;
        let prs = pr_summaries(&state.repo, &records)?;
        let mut stats = summary(&state, &records);
        stats["pull_requests"] = serde_json::to_value(&prs)?;
        summaries.insert(state.bot_login.clone(), stats);
        let csv_path = Path::new("out/automated-review-stats").join(format!(
            "{}_{}_{}.csv",
            config.org, config.repo, state.bot_login
        ));
        fs::create_dir_all(csv_path.parent().unwrap())?;
        let mut writer = csv::Writer::from_path(csv_path)?;
        for pr in &prs {
            writer.serialize(pr)?;
        }
        writer.flush()?;
    }
    let path = Path::new("out/summaries").join(format!(
        "{}_{}_automated_review_stats.json",
        config.org, config.repo
    ));
    write_json(
        &path,
        &json!({"bots":summaries.into_values().collect::<Vec<_>>()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        State {
            schema_version: 2,
            repo: "o/r".into(),
            bot_login: "bot".into(),
            since: "2026-01-01T00:00:00Z".into(),
            discovery_through: "2026-10-19T12:00:00Z".into(),
            collected_at: "2026-10-19T12:00:00Z".into(),
            prs_scanned: 2,
        }
    }
    fn sample(id: &str) -> Comment {
        Comment {
            id: id.into(),
            pr: 1,
            merged_at: "2026-02-01T00:00:00Z".into(),
            thumbs_up: 1,
            thumbs_down: 0,
        }
    }
    #[test]
    fn pr_totals_distinguish_comments_from_reactions() {
        let mut liked = sample("a");
        liked.thumbs_up = 10;
        let mut mixed = sample("b");
        mixed.thumbs_down = 2;
        let mut disliked = sample("c");
        disliked.pr = 2;
        disliked.thumbs_up = 0;
        disliked.thumbs_down = 3;
        let mut none = sample("d");
        none.thumbs_up = 0;
        let records = BTreeMap::from([
            (liked.id.clone(), liked),
            (mixed.id.clone(), mixed),
            (disliked.id.clone(), disliked),
            (none.id.clone(), none),
        ]);
        let prs = pr_summaries("o/r", &records).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].url, "https://github.com/o/r/pull/1");
        assert_eq!(
            (
                prs[0].total,
                prs[0].liked,
                prs[0].disliked,
                prs[0].no_signal,
                prs[0].mixed
            ),
            (3, 1, 0, 2, 1)
        );
        assert_eq!((prs[0].thumbs_up, prs[0].thumbs_down), (11, 2));
        assert_eq!(
            (prs[1].total, prs[1].disliked, prs[1].thumbs_down),
            (1, 1, 3)
        );
        let stats = summary(&state(), &records);
        assert_eq!(stats["total"], prs.iter().map(|p| p.total).sum::<usize>());
        assert_eq!(
            stats["no_signal"],
            prs.iter().map(|p| p.no_signal).sum::<usize>()
        );
    }

    #[test]
    fn compact_storage_groups_comments_and_rejects_conflicting_pr_dates() {
        let mut records = BTreeMap::from([("a".into(), sample("a")), ("b".into(), sample("b"))]);
        let grouped = group_by_pr(&records).unwrap();
        let json = serde_json::to_value(&grouped[&1]).unwrap();
        assert_eq!(json["comments"].as_array().unwrap().len(), 2);
        assert_eq!(json["comments"][0].as_object().unwrap().len(), 3);
        assert!(json["comments"][0].get("pr").is_none());
        records.get_mut("b").unwrap().merged_at = "2025-01-01T00:00:00Z".into();
        assert!(group_by_pr(&records).is_err());
    }

    #[test]
    fn coverage_and_watermark_are_independent() {
        let s = state();
        assert_eq!(cutoff(None, Some(&s)).unwrap(), s.since);
        assert!(cutoff(Some("2026-02-01"), Some(&s)).is_err());
        assert_eq!(
            discovery_boundary(&s.since, Some(&s), false).unwrap(),
            "2026-10-18T12:00:00Z"
        );
        assert_eq!(
            discovery_boundary(&s.since, Some(&s), true).unwrap(),
            s.since
        );
        assert_eq!(
            discovery_boundary("2025-01-01T00:00:00Z", Some(&s), false).unwrap(),
            "2025-01-01T00:00:00Z"
        );
    }
    #[test]
    fn reactions_refresh_from_minimal_records_and_deletions_are_removed() {
        let mut records = BTreeMap::from([("a".into(), sample("a")), ("b".into(), sample("b"))]);
        let value = json!([{"id":"a","author":{"login":"bot"},"up":{"totalCount":1},"down":{"totalCount":2}},null]);
        apply_refresh(&mut records, &["a".into(), "b".into()], &value, "bot").unwrap();
        let stats = summary(&state(), &records);
        assert_eq!(stats["total"], 1);
        assert_eq!(stats["mixed"], 1);
        assert_eq!(stats["no_signal"], 1);
        assert_eq!(stats["liked"], 0);
        assert_eq!(records["a"].merged_at, "2026-02-01T00:00:00Z");
        assert!(apply_refresh(&mut records, &["a".into()], &json!([]), "bot").is_err());
    }
    #[test]
    fn year_buckets_round_trip_and_clear_deleted_records() {
        let dir = std::env::temp_dir().join(format!("github-tools-ai-test-{}", std::process::id()));
        let mut old = sample("old");
        old.merged_at = "2025-12-31T00:00:00Z".into();
        old.pr = 2;
        let mut records = BTreeMap::from([("old".into(), old), ("new".into(), sample("new"))]);
        save(&dir, &records, &state()).unwrap();
        assert_eq!(read_records(&dir).unwrap(), records);
        records.remove("old");
        save(&dir, &records, &state()).unwrap();
        assert_eq!(read_records(&dir).unwrap(), records);
        fs::write(dir.join("2025.json"), "broken").unwrap();
        assert!(read_records(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn follows_both_nested_cursors_and_propagates_failed_pages() {
        let c = |id: &str| json!({"id":id,"author":{"login":"bot"},"up":{"totalCount":0},"down":{"totalCount":0}});
        let connection = |items: Value, cursor: Option<&str>| json!({"nodes":items,"pageInfo":{"hasNextPage":cursor.is_some(),"endCursor":cursor}});
        let pages = vec![
            json!({"repository":{"pullRequest":{"reviewThreads":connection(json!([{"id":"thread","comments":connection(json!([c("a")]),Some("comment-cursor"))}]),Some("thread-cursor"))}}}),
            json!({"node":{"comments":connection(json!([c("b")]),None)}}),
            json!({"repository":{"pullRequest":{"reviewThreads":connection(json!([{"id":"thread2","comments":connection(json!([c("c")]),None)}]),None)}}}),
        ];
        let config = Config {
            org: "o".into(),
            repo: "r".into(),
            github_token: String::new(),
        };
        let mut calls = 0;
        let records = fetch_comments(
            &config,
            1,
            "2026-02-01T00:00:00Z",
            Some("bot"),
            &BTreeMap::new(),
            &mut BTreeMap::new(),
            |q, v| {
                match calls {
                    0 => {
                        assert_eq!(q, THREADS);
                        assert!(v["after"].is_null());
                    }
                    1 => {
                        assert_eq!(q, COMMENTS);
                        assert_eq!(v["after"], "comment-cursor");
                    }
                    2 => {
                        assert_eq!(q, THREADS);
                        assert_eq!(v["after"], "thread-cursor");
                    }
                    _ => panic!("unexpected request"),
                }
                let page = pages[calls].clone();
                calls += 1;
                Ok(page)
            },
        )
        .unwrap();
        assert_eq!(records.len(), 3);
        let mut calls = 0;
        let result = fetch_comments(
            &config,
            1,
            "2026-02-01T00:00:00Z",
            Some("bot"),
            &BTreeMap::new(),
            &mut BTreeMap::new(),
            |_, _| {
                calls += 1;
                if calls == 1 {
                    Ok(pages[0].clone())
                } else {
                    bail!("rate limited")
                }
            },
        );
        assert!(result.is_err());
    }
    #[test]
    fn pagination_requires_a_cursor_and_counts_are_not_truncated() {
        assert!(next(&json!({"pageInfo":{"hasNextPage":true}})).is_err());
        let c = comment(
            &json!({"id":"x","up":{"totalCount":501},"down":{"totalCount":0}}),
            1,
            "2026-01-02T00:00:00Z",
        )
        .unwrap();
        assert_eq!(c.thumbs_up, 501);
    }
}
