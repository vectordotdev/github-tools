//! Materialize saved backlog observations once, instead of rebuilding them in the UI.
use super::{snapshots, sorted_json};
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Point {
    date: String,
    date_basis: String,
    coverage: Option<String>,
    source_revision: Option<String>,
    total: Option<u64>,
    reason: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    schema_version: u8,
    repository: String,
    generated_at: String,
    series: BTreeMap<String, Vec<Point>>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Collection {
    schema_version: u8,
    repository: String,
    collected_at: String,
    coverage: String,
    fingerprint: String,
}
#[derive(Clone)]
struct State {
    fingerprint: String,
    totals: [Option<u64>; 2],
}
fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git").current_dir(repo).args(args).output()?;
    ensure!(
        output.status.success(),
        "Could not read saved history: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}
fn fingerprint(repo: &Path, bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .current_dir(repo)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("Missing hash input")?
        .write_all(bytes)?;
    let output = child.wait_with_output()?;
    ensure!(output.status.success(), "Could not fingerprint saved data");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
fn year_file(path: &str) -> bool {
    path.strip_suffix(".json")
        .is_some_and(|name| name.len() == 4 && name.bytes().all(|c| c.is_ascii_digit()))
}
fn state(repo: &Path, files: &BTreeMap<String, Vec<u8>>, kind: &str) -> Result<State> {
    let mut canonical = BTreeMap::new();
    let mut rows = Vec::new();
    let mut valid = true;
    for (name, bytes) in files {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Array(mut year)) => {
                year.sort_by_key(|row| row["number"].as_u64());
                rows.extend(year.iter().cloned());
                canonical.insert(name.clone(), Value::Array(year));
            }
            _ => {
                valid = false;
                canonical.insert(name.clone(), json!(String::from_utf8_lossy(bytes)));
            }
        }
    }
    let fingerprint = fingerprint(repo, sorted_json(&canonical)?.as_bytes())?;
    let totals = if valid {
        count(&rows, kind).unwrap_or([None, None])
    } else {
        [None, None]
    };
    Ok(State {
        fingerprint,
        totals,
    })
}
fn count(rows: &[Value], kind: &str) -> Result<[Option<u64>; 2]> {
    let mut seen = HashSet::new();
    let mut totals = [Some(0), Some(0)];
    for row in rows {
        let number = row["number"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
            .context("Invalid number")?;
        ensure!(seen.insert(number), "Duplicate saved item");
        if kind == "issues" {
            ensure!(
                matches!(row["state"].as_str(), Some("open" | "closed")),
                "Invalid saved state"
            );
            if row["state"] != "open" {
                continue;
            }
            if row.get("pull_request").is_none() {
                *totals[0].as_mut().unwrap() += 1;
            } else {
                match row["draft"].as_bool() {
                    Some(false) => {
                        if let Some(total) = &mut totals[1] {
                            *total += 1;
                        }
                    }
                    Some(true) => (),
                    None => totals[1] = None,
                }
            }
        } else {
            let category = row["category"]["name"]
                .as_str()
                .context("Missing category")?;
            let closed = row["closed"].as_bool().context("Missing closed state")?;
            if !category.trim().eq_ignore_ascii_case("q&a") || closed {
                continue;
            }
            match row["isAnswered"].as_bool() {
                Some(true) => (),
                Some(false) => {
                    if let Some(total) = &mut totals[0] {
                        *total += 1;
                    }
                }
                None if row["isAnswered"].is_null() && row["category"]["isAnswerable"] == false => {
                    if let Some(total) = &mut totals[0] {
                        *total += 1;
                    }
                }
                _ => totals[0] = None,
            }
        }
    }
    Ok(totals)
}
fn metadata_path(root: &str, kind: &str) -> String {
    format!("{root}/collection-metadata/{kind}.json")
}
fn read_working(repo: &Path, root: &str, kind: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    let path = repo.join(root).join(kind);
    let mut files = BTreeMap::new();
    if path.exists() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .to_str()
                .context("Invalid year path")?
                .to_owned();
            if year_file(&name) {
                files.insert(name, fs::read(entry.path())?);
            }
        }
    }
    Ok(files)
}
fn collection(bytes: Option<&[u8]>, repository: &str, state: &State) -> Option<Collection> {
    let value: Collection = serde_json::from_slice(bytes?).ok()?;
    (value.schema_version == 1
        && value.repository == repository
        && value.fingerprint == state.fingerprint
        && ["full", "incremental"].contains(&value.coverage.as_str())
        && DateTime::parse_from_rfc3339(&value.collected_at).is_ok_and(|date| date <= Utc::now()))
    .then_some(value)
}
fn points(
    state: &State,
    date: &str,
    revision: Option<&str>,
    collection: Option<&Collection>,
) -> [Point; 2] {
    state.totals.map(|total| Point {
        date: collection.map_or(date, |c| &c.collected_at).to_owned(),
        date_basis: if collection.is_some() {
            "collected"
        } else {
            "saved"
        }
        .to_owned(),
        coverage: collection.map(|c| c.coverage.clone()),
        source_revision: revision.map(str::to_owned),
        total,
        reason: total
            .is_none()
            .then(|| "Incomplete saved snapshot".to_owned()),
    })
}
/// Record when a successful collector completed and bind that timestamp to its exact saved contents.
pub(super) fn record_collection(config: &Config, kind: &str, since: Option<&str>) -> Result<()> {
    let repo = Path::new(".");
    let root = format!("data/{}_{}", config.org, config.repo);
    let state = state(repo, &read_working(repo, &root, kind)?, kind)?;
    snapshots::save(
        &repo.join(metadata_path(&root, kind)),
        &Collection {
            schema_version: 1,
            repository: format!("{}/{}", config.org, config.repo),
            collected_at: Utc::now().to_rfc3339(),
            coverage: if since.is_some() {
                "incremental"
            } else {
                "full"
            }
            .to_owned(),
            fingerprint: state.fingerprint,
        },
    )
}
fn history(repo: &Path, root: &str, repository: &str, kind: &str) -> Result<Vec<[Point; 2]>> {
    let path = format!("{root}/{kind}");
    let meta_path = metadata_path(root, kind);
    let log = String::from_utf8(git(
        repo,
        &[
            "log",
            "-100",
            "--format=%H %cI",
            "HEAD",
            "--",
            &path,
            &meta_path,
        ],
    )?)?;
    let mut entries = log
        .lines()
        .map(|line| line.split_once(' ').context("Invalid history revision"))
        .collect::<Result<Vec<_>>>()?;
    entries.reverse();
    let mut blobs = HashMap::new();
    let mut previous = None;
    let mut result = Vec::new();
    for (sha, saved_at) in entries {
        let tree = String::from_utf8(git(repo, &["ls-tree", "-r", sha, "--", &path, &meta_path])?)?;
        let mut files = BTreeMap::new();
        let mut metadata = None;
        for line in tree.lines() {
            let (header, file) = line.split_once('\t').context("Invalid source tree")?;
            let blob = header
                .split_whitespace()
                .nth(2)
                .context("Missing source blob")?;
            if file != meta_path
                && !file
                    .strip_prefix(&format!("{path}/"))
                    .is_some_and(year_file)
            {
                continue;
            }
            if !blobs.contains_key(blob) {
                blobs.insert(blob.to_owned(), git(repo, &["cat-file", "blob", blob])?);
            }
            let bytes = &blobs[blob];
            if file == meta_path {
                metadata = Some(bytes.clone());
            } else {
                files.insert(file.rsplit('/').next().unwrap().to_owned(), bytes.clone());
            }
        }
        let mut state = state(repo, &files, kind)?;
        let metadata = collection(metadata.as_deref(), repository, &state);
        // Absent year files are unavailable, except for a successful empty collection.
        if files.is_empty() && metadata.is_none() {
            state.totals = [None, None];
        }
        let signature = (
            state.fingerprint.clone(),
            metadata.as_ref().map(|c| c.collected_at.clone()),
        );
        if previous.as_ref() == Some(&signature) {
            continue;
        } // Formatting-only commits are not observations.
        let date = DateTime::parse_from_rfc3339(saved_at)?
            .with_timezone(&Utc)
            .to_rfc3339();
        result.push(points(&state, &date, Some(sha), metadata.as_ref()));
        previous = Some(signature);
    }
    let files = read_working(repo, root, kind)?;
    let state = state(repo, &files, kind)?;
    let bytes = fs::read(repo.join(meta_path)).ok();
    let metadata = collection(bytes.as_deref(), repository, &state);
    let signature = (
        state.fingerprint.clone(),
        metadata.as_ref().map(|c| c.collected_at.clone()),
    );
    if previous.as_ref() != Some(&signature) {
        if let Some(metadata) = metadata {
            result.push(points(
                &state,
                &metadata.collected_at,
                None,
                Some(&metadata),
            ));
        } else {
            ensure!(
                previous.is_none() && files.is_empty(),
                "Uncommitted {kind} changes need a successful collection timestamp or a source commit"
            );
        }
    }
    // Keep the last observation per UTC day, including a final unavailable state.
    let mut days = BTreeMap::new();
    for point in result {
        days.insert(point[0].date[..10].to_owned(), point);
    }
    Ok(days.into_values().collect())
}
fn build(repo: &Path, root: &str, repository: &str) -> Result<Snapshot> {
    ensure!(
        String::from_utf8(git(repo, &["rev-parse", "--is-shallow-repository"])?)?.trim() == "false",
        "Full Git history is required to export backlog history"
    );
    let issues = history(repo, root, repository, "issues")?;
    let discussions = history(repo, root, repository, "discussions")?;
    Ok(Snapshot {
        schema_version: 1,
        repository: repository.to_owned(),
        generated_at: Utc::now().to_rfc3339(),
        series: BTreeMap::from([
            (
                "issues".to_owned(),
                issues.iter().map(|p| p[0].clone()).collect(),
            ),
            (
                "prs".to_owned(),
                issues.iter().map(|p| p[1].clone()).collect(),
            ),
            (
                "discussions".to_owned(),
                discussions.iter().map(|p| p[0].clone()).collect(),
            ),
        ]),
    })
}
// Share observation dates with the contributor collector so formatting commits
// cannot reappear as fresh Team & Community measurements on its next collection.
pub(super) fn issue_revisions(config: &Config) -> Result<Vec<(String, String)>> {
    let root = format!("data/{}_{}", config.org, config.repo);
    Ok(history(
        Path::new("."),
        &root,
        &format!("{}/{}", config.org, config.repo),
        "issues",
    )?
    .into_iter()
    .filter_map(|points| {
        let point = &points[0];
        point
            .source_revision
            .as_ref()
            .map(|sha| (sha.clone(), point.date.clone()))
    })
    .collect())
}
pub fn run(config: &Config) -> Result<()> {
    let root = format!("data/{}_{}", config.org, config.repo);
    let snapshot = build(
        Path::new("."),
        &root,
        &format!("{}/{}", config.org, config.repo),
    )?;
    snapshots::save(
        &Path::new(&root).join("backlog-history/latest.json"),
        &snapshot,
    )?;
    println!(
        "{}: saved {} backlog observations",
        snapshot.repository,
        snapshot.series.values().map(Vec::len).sum::<usize>()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pr(number: u64, draft: Value) -> Value {
        json!({"number":number,"state":"open","pull_request":{},"draft":draft})
    }
    fn discussion(number: u64, extra: Value) -> Value {
        let mut row =
            json!({"number":number,"closed":false,"isAnswered":false,"category":{"name":"Q&A"}});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }
    #[test]
    fn counts_preserve_zero_unknown_drafts_duplicates_and_qa_semantics() {
        assert_eq!(
            count(
                &[
                    json!({"number":1,"state":"open"}),
                    pr(2, json!(false)),
                    pr(3, json!(true)),
                    json!({"number":4,"state":"closed"})
                ],
                "issues"
            )
            .unwrap(),
            [Some(1), Some(1)]
        );
        assert_eq!(
            count(&[pr(1, Value::Null)], "issues").unwrap(),
            [Some(0), None]
        );
        assert!(count(&[pr(1, json!(false)), pr(1, json!(false))], "issues").is_err());
        assert!(count(&[json!({"number":1,"state":"invalid"})], "issues").is_err());
        assert_eq!(
            count(
                &[
                    discussion(1, json!({})),
                    discussion(2, json!({"isAnswered":true})),
                    discussion(3, json!({"closed":true})),
                    discussion(4, json!({"category":{"name":"General"}}))
                ],
                "discussions"
            )
            .unwrap()[0],
            Some(1)
        );
        assert_eq!(
            count(&[discussion(1, json!({"isAnswered":null}))], "discussions").unwrap()[0],
            None
        );
        assert_eq!(count(&[], "issues").unwrap(), [Some(0), Some(0)]);
    }
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "backlog-history-{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap()
            ));
            fs::create_dir_all(&path).unwrap();
            let fixture = Self(path);
            git(&fixture.0, &["init", "-q"]).unwrap();
            git(&fixture.0, &["config", "user.name", "Test"]).unwrap();
            git(&fixture.0, &["config", "user.email", "test@example.com"]).unwrap();
            fixture
        }
        fn write(&self, path: &str, value: &str) {
            let file = self.0.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, value).unwrap();
        }
        fn commit(&self, date: &str) {
            // Synthetic fixture objects avoid invoking the user's hooks or signer.
            git(&self.0, &["add", "."]).unwrap();
            let tree = String::from_utf8(git(&self.0, &["write-tree"]).unwrap()).unwrap();
            let parent = Command::new("git")
                .current_dir(&self.0)
                .args(["rev-parse", "--verify", "HEAD"])
                .output()
                .unwrap();
            let parent = if parent.status.success() {
                format!(
                    "parent {}\n",
                    String::from_utf8(parent.stdout).unwrap().trim()
                )
            } else {
                String::new()
            };
            let timestamp = DateTime::parse_from_rfc3339(date).unwrap().timestamp();
            let object = format!(
                "tree {}\n{parent}author Test <test@example.com> {timestamp} +0000\ncommitter Test <test@example.com> {timestamp} +0000\n\nfixture\n",
                tree.trim()
            );
            let mut child = Command::new("git")
                .current_dir(&self.0)
                .args(["hash-object", "-t", "commit", "-w", "--stdin"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(object.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            let sha = String::from_utf8(output.stdout).unwrap();
            git(&self.0, &["update-ref", "HEAD", sha.trim()]).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn formatting_does_not_redate_history_and_invalid_states_remain_gaps() {
        let f = Fixture::new();
        let root = "data/example_repo";
        f.write(
            &format!("{root}/issues/2026.json"),
            r#"[{"state":"open","number":1}]"#,
        );
        f.commit("2026-09-03T12:00:00Z");
        f.write(
            &format!("{root}/issues/2026.json"),
            r#"[ { "number": 1, "state": "open" } ]"#,
        );
        f.commit("2026-10-21T12:00:00Z");
        let history = build(&f.0, root, "example/repo").unwrap();
        assert_eq!(history.series["issues"].len(), 1);
        assert!(history.series["issues"][0].date.starts_with("2026-09-03"));
        assert_eq!(history.series["issues"][0].date_basis, "saved");
        f.write(
            &format!("{root}/issues/2025.json"),
            r#"[{"number":1,"state":"open"}]"#,
        );
        f.commit("2026-11-02T12:00:00Z");
        let history = build(&f.0, root, "example/repo").unwrap();
        assert_eq!(history.series["issues"].last().unwrap().total, None);
    }
    #[test]
    fn collection_timestamp_requires_matching_contents_and_uncommitted_changes_are_rejected() {
        let f = Fixture::new();
        let root = "data/example_repo";
        let file = format!("{root}/issues/2026.json");
        f.write(&file, r#"[{"number":1,"state":"open"}]"#);
        f.commit("2026-09-03T12:00:00Z");
        let state = state(&f.0, &read_working(&f.0, root, "issues").unwrap(), "issues").unwrap();
        let metadata = Collection {
            schema_version: 1,
            repository: "example/repo".into(),
            collected_at: "2026-11-08T12:00:00Z".into(),
            coverage: "full".into(),
            fingerprint: state.fingerprint,
        };
        snapshots::save(&f.0.join(metadata_path(root, "issues")), &metadata).unwrap();
        let history = build(&f.0, root, "example/repo").unwrap();
        let point = history.series["issues"].last().unwrap();
        assert_eq!(point.date_basis, "collected");
        assert!(point.source_revision.is_none());
        assert_eq!(point.date, metadata.collected_at);
        f.write(&file, r#"[{"number":1,"state":"closed"}]"#);
        assert!(build(&f.0, root, "example/repo").is_err());
        f.commit("2026-11-09T12:00:00Z");
        let history = build(&f.0, root, "example/repo").unwrap();
        let point = history.series["issues"].last().unwrap();
        assert_eq!(point.date_basis, "saved");
        assert_eq!(point.total, Some(0));
    }
    #[test]
    fn equal_totals_and_fresh_collections_remain_observations_but_formatting_does_not() {
        let f = Fixture::new();
        let root = "data/example_repo";
        let file = format!("{root}/issues/2026.json");
        f.write(&file, r#"[{"number":1,"state":"open","title":"before"}]"#);
        f.commit("2026-09-03T12:00:00Z");
        f.write(&file, r#"[{"number":1,"state":"open","title":"after"}]"#);
        f.commit("2026-11-02T12:00:00Z");
        let state = state(&f.0, &read_working(&f.0, root, "issues").unwrap(), "issues").unwrap();
        let metadata = Collection {
            schema_version: 1,
            repository: "example/repo".into(),
            collected_at: "2026-11-08T12:00:00Z".into(),
            coverage: "incremental".into(),
            fingerprint: state.fingerprint,
        };
        snapshots::save(&f.0.join(metadata_path(root, "issues")), &metadata).unwrap();
        f.commit("2026-11-08T13:00:00Z");
        f.write(
            &file,
            r#"[ { "title": "after", "state": "open", "number": 1 } ]"#,
        );
        f.commit("2026-11-09T12:00:00Z");
        let snapshot = build(&f.0, root, "example/repo").unwrap();
        let points = &snapshot.series["issues"];
        assert_eq!(points.len(), 3);
        assert!(points.iter().all(|p| p.total == Some(1)));
        assert_eq!(points[2].date, metadata.collected_at);
        assert_eq!(points[2].date_basis, "collected");
        assert_eq!(points[2].coverage.as_deref(), Some("incremental"));
        assert!(snapshot.series["discussions"].is_empty());
    }
}
