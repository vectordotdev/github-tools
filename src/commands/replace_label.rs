//! Rename a label, or merge its assignments into an existing destination label.
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RESOLVE: &str = r#"
query ResolveLabels($owner: String!, $repo: String!, $old: String!, $new: String!) {
  repository(owner: $owner, name: $repo) {
    hasDiscussionsEnabled
    old: label(name: $old) { id name }
    new: label(name: $new) { id name }
  }
}"#;

const READ_LABELS: &str = r#"
query ItemLabels($id: ID!, $after: String) {
  node(id: $id) {
    ... on Labelable {
      viewerCanLabel
      labels(first: 100, after: $after) {
        nodes { id }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}"#;

const ADD: &str = r#"
mutation AddReplacement($item: ID!, $label: ID!) {
  change: addLabelsToLabelable(input: {labelableId: $item, labelIds: [$label]}) {
    labelable { ... on Node { id } }
  }
}"#;

const REMOVE: &str = r#"
mutation RemoveOriginal($item: ID!, $label: ID!) {
  change: removeLabelsFromLabelable(input: {labelableId: $item, labelIds: [$label]}) {
    labelable { ... on Node { id } }
  }
}"#;

const RENAME: &str = r#"
mutation RenameLabel($id: ID!, $name: String!) {
  updateLabel(input: {id: $id, name: $name}) { label { id name } }
}"#;

trait Api {
    fn request(&mut self, query: &str, variables: Value) -> Result<Value>;
}

struct GitHub<'a> {
    client: Client,
    token: &'a str,
    last_mutation: Option<Instant>,
}

impl Api for GitHub<'_> {
    fn request(&mut self, query: &str, variables: Value) -> Result<Value> {
        if query.trim_start().starts_with("mutation") {
            // Serial writes, spaced to reduce secondary rate-limit pressure.
            if let Some(last) = self.last_mutation {
                std::thread::sleep(Duration::from_secs(1).saturating_sub(last.elapsed()));
            }
            self.last_mutation = Some(Instant::now());
        }
        // Do not blindly retry an ambiguous mutation. A rerun discovers current state.
        if self.token.is_empty() {
            // Delegate authentication to gh; never extract or persist its token.
            let mut child = Command::new("gh")
                .args(["api", "graphql", "--hostname", "github.com", "--input", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("GITHUB_TOKEN is not set and gh could not be started")?;
            let body = serde_json::to_vec(&json!({"query": query, "variables": variables}))?;
            let written = child
                .stdin
                .take()
                .context("Missing gh stdin")?
                .write_all(&body);
            let output = child.wait_with_output().context("Failed to wait for gh")?;
            ensure!(
                output.status.success(),
                "GitHub request through gh failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            written.context("Failed to send request to gh")?;
            return decode_response(
                serde_json::from_slice(&output.stdout).context("Invalid gh response")?,
            );
        }
        let response = self
            .client
            .post("https://api.github.com/graphql")
            .bearer_auth(self.token)
            .header("User-Agent", "github-tools")
            .json(&json!({"query": query, "variables": variables}))
            .send()
            .context(
                "GitHub request failed; any mutation outcome may be unknown. Rerun to reconcile",
            )?;
        let status = response.status();
        ensure!(
            status.is_success(),
            "GitHub returned {status}; stopped without retrying. Rerun after resolving access or rate limits"
        );
        decode_response(response.json().context("Invalid GitHub response")?)
    }
}

fn decode_response(response: Value) -> Result<Value> {
    if let Some(errors) = response.get("errors").filter(|e| !e.is_null()) {
        ensure!(
            errors.as_array().is_some_and(Vec::is_empty),
            "GitHub GraphQL errors: {errors}"
        );
    }
    response
        .get("data")
        .filter(|d| !d.is_null())
        .cloned()
        .context("Missing GraphQL data")
}

#[derive(Debug, Deserialize)]
struct Label {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Id {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    nodes: Vec<T>,
    page_info: PageInfo,
}

// A broken cursor must fail closed rather than silently truncating a migration.
fn next_cursor(info: PageInfo, seen: &mut HashSet<String>) -> Result<Option<String>> {
    if !info.has_next_page {
        return Ok(None);
    }
    let cursor = info
        .end_cursor
        .filter(|s| !s.is_empty())
        .context("Missing pagination cursor")?;
    ensure!(seen.insert(cursor.clone()), "Repeated pagination cursor");
    Ok(Some(cursor))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LabelState {
    viewer_can_label: bool,
    labels: Page<Id>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: String,
    #[serde(rename = "__typename")]
    kind: String,
    number: u64,
    title: String,
    url: String,
    viewer_can_label: bool,
}

#[derive(Debug, Deserialize)]
struct InventoryItem {
    #[serde(flatten)]
    item: Item,
    labels: Page<Id>,
}

#[derive(Debug)]
enum Destination {
    Existing(Label),
    Rename(String),
}

impl Destination {
    fn name(&self) -> &str {
        match self {
            Self::Existing(label) => &label.name,
            Self::Rename(name) => name,
        }
    }
}

#[derive(Debug)]
struct Plan {
    old: Label,
    new: Destination,
    discussions_enabled: bool,
    items: Vec<Item>,
}

fn all_labels(api: &mut impl Api, id: &str, first: LabelState) -> Result<(bool, HashSet<String>)> {
    let mut labels = HashSet::new();
    let mut seen = HashSet::new();
    let mut state = first;
    let mut can_label = true;
    loop {
        can_label &= state.viewer_can_label;
        labels.extend(state.labels.nodes.into_iter().map(|l| l.id));
        let Some(after) = next_cursor(state.labels.page_info, &mut seen)? else {
            return Ok((can_label, labels));
        };
        let data = api.request(READ_LABELS, json!({"id": id, "after": after}))?;
        state =
            serde_json::from_value(data["node"].clone()).context("Missing item or label page")?;
    }
}

fn read_labels(api: &mut impl Api, id: &str) -> Result<(bool, HashSet<String>)> {
    let data = api.request(READ_LABELS, json!({"id": id, "after": null}))?;
    let state = serde_json::from_value(data["node"].clone()).context("Missing item or labels")?;
    all_labels(api, id, state)
}

fn collect_items(
    api: &mut impl Api,
    config: &Config,
    old: &Label,
    discussions: bool,
) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    let mut ids = HashSet::new();
    for connection in ["issues", "pullRequests", "discussions"] {
        if connection == "discussions" && !discussions {
            continue;
        }
        // Discussion connections have no label filter. Scan them all, including
        // closed discussions. Label connections likewise omit a state filter so
        // closed issues and closed/merged PRs are included.
        let (head, tail, variables) = if connection == "discussions" {
            (
                "query Discover($owner: String!, $repo: String!, $after: String) { scope: repository(owner: $owner, name: $repo) {".to_owned(),
                "}}",
                json!({"owner": config.org, "repo": config.repo}),
            )
        } else {
            (
                "query Discover($label: ID!, $after: String) { scope: node(id: $label) { ... on Label {".to_owned(),
                "}}}",
                json!({"label": old.id}),
            )
        };
        let query = format!(
            r#"{head}
            items: {connection}(first: 100, after: $after, orderBy: {{field: CREATED_AT, direction: ASC}}) {{
                nodes {{
                    id __typename number title url viewerCanLabel
                    labels(first: 100) {{ nodes {{ id }} pageInfo {{ hasNextPage endCursor }} }}
                }}
                pageInfo {{ hasNextPage endCursor }}
            }}
        {tail}"#
        );
        let mut after = None::<String>;
        let mut cursors = HashSet::new();
        loop {
            let mut vars = variables.clone();
            vars["after"] = json!(after);
            let data = api.request(&query, vars)?;
            let page: Page<InventoryItem> = serde_json::from_value(data["scope"]["items"].clone())
                .with_context(|| format!("Incomplete {connection} inventory"))?;
            for entry in page.nodes {
                let mut item = entry.item;
                let initial = LabelState {
                    viewer_can_label: item.viewer_can_label,
                    labels: entry.labels,
                };
                let (can_label, labels) = all_labels(api, &item.id, initial)?;
                item.viewer_can_label = can_label;
                if labels.contains(&old.id) && ids.insert(item.id.clone()) {
                    items.push(item);
                }
            }
            after = next_cursor(page.page_info, &mut cursors)?;
            if after.is_none() {
                break;
            }
        }
    }
    Ok(items)
}

fn plan(api: &mut impl Api, config: &Config, old: &str, new: &str) -> Result<Plan> {
    ensure!(
        !old.trim().is_empty() && !new.trim().is_empty(),
        "Label names cannot be empty"
    );
    let data = api.request(
        RESOLVE,
        json!({"owner": config.org, "repo": config.repo, "old": old, "new": new}),
    )?;
    let repo = &data["repository"];
    let old: Label = serde_json::from_value(repo["old"].clone())
        .context("Source label does not exist or repository is inaccessible")?;
    let destination = repo
        .get("new")
        .context("Missing destination lookup result")?;
    let new = if destination.is_null() {
        Destination::Rename(new.to_owned())
    } else {
        let label: Label =
            serde_json::from_value(destination.clone()).context("Invalid destination label")?;
        ensure!(
            old.id != label.id,
            "Source and destination resolve to the same label"
        );
        Destination::Existing(label)
    };
    let discussions_enabled = repo["hasDiscussionsEnabled"]
        .as_bool()
        .context("Missing discussion availability")?;
    let items = collect_items(api, config, &old, discussions_enabled)?;
    Ok(Plan {
        old,
        new,
        discussions_enabled,
        items,
    })
}

fn change_label(api: &mut impl Api, query: &str, item: &Item, label: &str) -> Result<()> {
    let data = api.request(query, json!({"item": item.id, "label": label}))?;
    ensure!(
        data["change"]["labelable"]["id"].as_str() == Some(&item.id),
        "GitHub did not confirm the label mutation"
    );
    Ok(())
}

fn apply_plan(api: &mut impl Api, plan: &Plan) -> Result<()> {
    let Destination::Existing(new) = &plan.new else {
        bail!("Assignment migration requires an existing destination label");
    };
    ensure!(
        plan.items.iter().all(|i| i.viewer_can_label),
        "Cannot edit labels on every planned item; no changes made"
    );
    println!("Phase 1: add and verify the destination label on every matching item.");
    for item in &plan.items {
        let result = (|| -> Result<()> {
            let (can_label, labels) = read_labels(api, &item.id)?;
            if !labels.contains(&plan.old.id) {
                return Ok(());
            }
            ensure!(can_label, "Label editing is no longer allowed");
            if !labels.contains(&new.id) {
                change_label(api, ADD, item, &new.id)?;
                let (_, labels) = read_labels(api, &item.id)?;
                ensure!(
                    labels.contains(&new.id),
                    "Destination label was not verified after addition"
                );
            }
            Ok(())
        })();
        result.with_context(|| {
            format!(
                "Addition failed for {}. No source labels have been removed; rerun to reconcile",
                item.url
            )
        })?;
    }

    println!("Phase 2: recheck the destination, remove the source, and verify each result.");
    for item in &plan.items {
        let result = (|| -> Result<()> {
            let (can_label, labels) = read_labels(api, &item.id)?;
            if !labels.contains(&plan.old.id) {
                println!("SKIP {} (source label already absent)", item.url);
                return Ok(());
            }
            ensure!(can_label, "Label editing is no longer allowed");
            ensure!(
                labels.contains(&new.id),
                "Destination label is missing; source label retained"
            );
            change_label(api, REMOVE, item, &plan.old.id)?;
            let (_, labels) = read_labels(api, &item.id)?;
            ensure!(
                labels.contains(&new.id) && !labels.contains(&plan.old.id),
                "Replacement could not be verified; concurrent edits may have occurred"
            );
            println!("REPLACED {}", item.url);
            Ok(())
        })();
        result.with_context(|| {
            format!(
                "Removal phase stopped at {}. Earlier changes are preserved; rerun to reconcile",
                item.url
            )
        })?;
    }
    Ok(())
}

fn rename_label(api: &mut impl Api, config: &Config, old: &Label, name: &str) -> Result<()> {
    let variables = json!({"owner": config.org, "repo": config.repo, "old": old.name, "new": name});
    // The preview can be stale by the time the user confirms. Never silently
    // switch from a rename to an assignment merge when the destination appears.
    let before = api.request(RESOLVE, variables.clone())?;
    ensure!(
        before["repository"]["old"]["id"].as_str() == Some(&old.id)
            && before["repository"]["old"]["name"].as_str() == Some(&old.name),
        "Source label changed since the preview; rerun to collect a fresh plan"
    );
    ensure!(
        before["repository"].get("new").is_some_and(Value::is_null),
        "Destination appeared since the preview; rename aborted"
    );
    let data = api.request(RENAME, json!({"id": old.id, "name": name}))?;
    ensure!(
        data["updateLabel"]["label"]["id"].as_str() == Some(&old.id)
            && data["updateLabel"]["label"]["name"].as_str() == Some(name),
        "Rename was not confirmed; inspect the current label names before retrying"
    );
    let after = api.request(RESOLVE, variables)?;
    ensure!(
        after["repository"].get("old").is_some_and(Value::is_null)
            && after["repository"]["new"]["id"].as_str() == Some(&old.id)
            && after["repository"]["new"]["name"].as_str() == Some(name),
        "Rename could not be verified; inspect the current label names before retrying"
    );
    println!(
        "Verified: renamed {:?} -> {:?}, retaining the same label ID and its assignments.",
        old.name, name
    );
    Ok(())
}

fn execute(
    api: &mut impl Api,
    config: &Config,
    old: &str,
    new: &str,
    apply: bool,
    yes: bool,
) -> Result<()> {
    let plan = plan(api, config, old, new)?;
    println!(
        "Replace {:?} -> {:?} in {}/{} (all states)",
        plan.old.name,
        plan.new.name(),
        config.org,
        config.repo
    );
    match &plan.new {
        Destination::Rename(_) => println!(
            "Action: rename the existing label, preserving its ID, color, description and assignments."
        ),
        Destination::Existing(_) => {
            println!("Action: merge assignments into the existing destination label.")
        }
    }
    for kind in ["Issue", "PullRequest", "Discussion"] {
        println!(
            "  {kind}: {}",
            plan.items.iter().filter(|i| i.kind == kind).count()
        );
    }
    if !plan.discussions_enabled {
        println!("Discussions are disabled; their assignments cannot be inspected or migrated.");
    }
    for item in &plan.items {
        println!(
            "  {} #{} {} {:?}{}",
            item.kind,
            item.number,
            item.url,
            item.title,
            if item.viewer_can_label {
                ""
            } else {
                " [NO LABEL PERMISSION]"
            }
        );
    }
    if !apply {
        println!("Preview only. Rerun with --apply to collect a fresh plan and confirm it.");
        return Ok(());
    }
    if !crate::confirm("Apply this label replacement?", yes) {
        println!("Aborted; no changes made.");
        return Ok(());
    }
    if let Destination::Rename(name) = &plan.new {
        return rename_label(api, config, &plan.old, name);
    }
    apply_plan(api, &plan)?;
    // Discover again: labels assigned during the run must not be silently missed.
    let remaining = collect_items(api, config, &plan.old, plan.discussions_enabled)
        .context("Changes applied, but the final inventory could not be completed")?;
    if !remaining.is_empty() {
        for item in &remaining {
            eprintln!("REMAINING {}", item.url);
        }
        bail!(
            "{} source-label assignments remain; rerun to reconcile concurrent changes",
            remaining.len()
        );
    }
    println!(
        "Verified: no source-label assignments remain in the scanned item types. Both repository label definitions are retained."
    );
    Ok(())
}

pub fn run(config: &Config, old: &str, new: &str, apply: bool, yes: bool) -> Result<()> {
    let mut api = GitHub {
        client: Client::builder().timeout(Duration::from_secs(60)).build()?,
        token: &config.github_token,
        last_mutation: None,
    };
    execute(&mut api, config, old, new, apply, yes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Script {
        steps: VecDeque<(&'static str, Value, Result<Value>)>,
    }

    impl Script {
        fn new() -> Self {
            Self {
                steps: VecDeque::new(),
            }
        }
        fn expect(&mut self, query: &'static str, vars: Value, result: Value) {
            self.steps.push_back((query, vars, Ok(result)));
        }
        fn fail(&mut self, query: &'static str, vars: Value) {
            self.steps
                .push_back((query, vars, Err(anyhow::anyhow!("injected API failure"))));
        }
        fn read(&mut self, id: &str, labels: &[&str]) {
            self.expect(
                "query ItemLabels",
                json!({"id": id, "after": null}),
                json!({"node": state(labels)}),
            );
        }
        fn change(&mut self, action: &'static str, id: &str, label: &str) {
            self.expect(
                action,
                json!({"item": id, "label": label}),
                json!({"change":{"labelable":{"id": id}}}),
            );
        }
        fn done(&self) {
            assert!(
                self.steps.is_empty(),
                "Unconsumed requests: {:?}",
                self.steps
            );
        }
    }

    impl Api for Script {
        fn request(&mut self, query: &str, vars: Value) -> Result<Value> {
            let (expected, expected_vars, result) =
                self.steps.pop_front().expect("Unexpected API request");
            assert!(query.contains(expected), "Expected {expected}, got {query}");
            for (key, value) in expected_vars.as_object().unwrap() {
                assert_eq!(&vars[key], value, "Variable {key}");
            }
            result
        }
    }

    fn config() -> Config {
        Config {
            github_token: String::new(),
            org: "owner".into(),
            repo: "repo".into(),
        }
    }
    fn page(nodes: Value) -> Value {
        json!({"nodes": nodes, "pageInfo": {"hasNextPage": false, "endCursor": null}})
    }
    fn state(labels: &[&str]) -> Value {
        json!({"viewerCanLabel": true, "labels": page(json!(labels.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()))})
    }
    fn item(id: &str, kind: &str, labels: &[&str]) -> Value {
        let mut value = state(labels);
        value["id"] = json!(id);
        value["__typename"] = json!(kind);
        value["number"] = json!(1);
        value["title"] = json!("A title");
        value["url"] = json!(format!("https://github.com/owner/repo/{kind}/{id}"));
        value
    }
    fn sample_plan() -> Plan {
        Plan {
            old: Label {
                id: "old".into(),
                name: "old".into(),
            },
            new: Destination::Existing(Label {
                id: "new".into(),
                name: "new".into(),
            }),
            discussions_enabled: true,
            items: vec![
                serde_json::from_value(item("a", "Issue", &["old"])).unwrap(),
                serde_json::from_value(item("b", "Discussion", &["old", "new"])).unwrap(),
            ],
        }
    }
    fn resolve(api: &mut Script, old_id: &str, new_id: &str) {
        api.expect("query ResolveLabels", json!({"old": "old", "new": "new"}), json!({"repository": {
            "old": {"id": old_id, "name": "old"}, "new": {"id": new_id, "name": "new"}, "hasDiscussionsEnabled": true
        }}));
    }
    fn inventory(api: &mut Script, issue: Value, pr: Value, discussion: Value) {
        for (connection, nodes) in [
            ("items: issues(", issue),
            ("items: pullRequests(", pr),
            ("items: discussions(", discussion),
        ] {
            api.expect(
                connection,
                json!({"after": null}),
                json!({"scope": {"items": page(nodes)}}),
            );
        }
    }

    #[test]
    fn adds_everywhere_before_removal_and_resumes_items_with_both_labels() {
        let mut api = Script::new();
        api.read("a", &["old", "unrelated"]);
        api.change("mutation AddReplacement", "a", "new");
        api.read("a", &["old", "new", "unrelated"]);
        api.read("b", &["old", "new"]); // No duplicate addition on a rerun.
        for id in ["a", "b"] {
            api.read(id, &["old", "new", "unrelated"]);
            api.change("mutation RemoveOriginal", id, "old");
            api.read(id, &["new", "unrelated"]);
        }
        apply_plan(&mut api, &sample_plan()).unwrap();
        api.done();
    }

    #[test]
    fn failed_addition_never_removes_any_source_labels() {
        let mut api = Script::new();
        api.read("a", &["old", "new"]);
        api.read("b", &["old"]);
        api.fail(
            "mutation AddReplacement",
            json!({"item": "b", "label": "new"}),
        );
        let error = apply_plan(&mut api, &sample_plan()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("No source labels have been removed")
        );
        api.done();
    }

    #[test]
    fn unverified_addition_stops_before_removal() {
        let mut api = Script::new();
        api.read("a", &["old"]);
        api.change("mutation AddReplacement", "a", "new");
        api.read("a", &["old"]);
        assert!(apply_plan(&mut api, &sample_plan()).is_err());
        api.done();
    }

    #[test]
    fn destination_removed_concurrently_prevents_source_removal() {
        let mut api = Script::new();
        api.read("a", &["old", "new"]);
        api.read("b", &["old", "new"]);
        api.read("a", &["old"]);
        let error = apply_plan(&mut api, &sample_plan()).unwrap_err();
        assert!(format!("{error:#}").contains("source label retained"));
        api.done();
    }

    #[test]
    fn ambiguous_removal_stops_and_preserves_earlier_changes() {
        let mut api = Script::new();
        api.read("a", &["old", "new"]);
        api.read("b", &["old", "new"]);
        api.read("a", &["old", "new"]);
        api.fail(
            "mutation RemoveOriginal",
            json!({"item": "a", "label": "old"}),
        );
        assert!(apply_plan(&mut api, &sample_plan()).is_err());
        api.done();
    }

    #[test]
    fn missing_permission_prevents_all_writes() {
        let mut plan = sample_plan();
        plan.items[1].viewer_can_label = false;
        let mut api = Script::new();
        assert!(apply_plan(&mut api, &plan).is_err());
        api.done();
    }

    #[test]
    fn source_removed_concurrently_is_not_reintroduced() {
        let mut api = Script::new();
        for _ in 0..2 {
            api.read("a", &["new"]);
            api.read("b", &[]);
        }
        apply_plan(&mut api, &sample_plan()).unwrap();
        api.done();
    }

    #[test]
    fn preview_collects_all_types_without_mutations() {
        let mut api = Script::new();
        resolve(&mut api, "old", "new");
        inventory(
            &mut api,
            json!([item("a", "Issue", &["old"])]),
            json!([item("b", "PullRequest", &["old"])]),
            json!([
                item("c", "Discussion", &["old"]),
                item("d", "Discussion", &["other"])
            ]),
        );
        execute(&mut api, &config(), "old", "new", false, false).unwrap();
        api.done();
    }

    #[test]
    fn paginates_items_and_discussion_labels_before_building_plan() {
        let mut api = Script::new();
        resolve(&mut api, "old", "new");
        let mut first = page(json!([item("a", "Issue", &["old"])]));
        first["pageInfo"] = json!({"hasNextPage":true, "endCursor":"page2"});
        api.expect(
            "items: issues(",
            json!({"after":null}),
            json!({"scope":{"items":first}}),
        );
        api.expect(
            "items: issues(",
            json!({"after":"page2"}),
            json!({"scope":{"items":page(json!([item("b", "Issue", &["old"])]))}}),
        );
        api.expect(
            "items: pullRequests(",
            json!({}),
            json!({"scope":{"items":page(json!([]))}}),
        );
        let mut discussion = item("c", "Discussion", &["other"]);
        discussion["labels"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"labels2"});
        api.expect(
            "items: discussions(",
            json!({}),
            json!({"scope":{"items":page(json!([discussion]))}}),
        );
        api.expect(
            "query ItemLabels",
            json!({"id":"c","after":"labels2"}),
            json!({"node":state(&["old"])}),
        );
        let plan = plan(&mut api, &config(), "old", "new").unwrap();
        assert_eq!(
            plan.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        api.done();
    }

    #[test]
    fn incomplete_inventory_aborts_apply_without_writes() {
        let mut api = Script::new();
        resolve(&mut api, "old", "new");
        api.expect(
            "items: issues(",
            json!({}),
            json!({"scope":{"items":page(json!([item("a", "Issue", &["old"])]))}}),
        );
        api.fail("items: pullRequests(", json!({}));
        assert!(execute(&mut api, &config(), "old", "new", true, true).is_err());
        api.done();
    }

    #[test]
    fn final_inventory_reports_assignments_created_during_run() {
        let mut api = Script::new();
        resolve(&mut api, "old", "new");
        inventory(&mut api, json!([]), json!([]), json!([]));
        inventory(
            &mut api,
            json!([item("late", "Issue", &["old"])]),
            json!([]),
            json!([]),
        );
        let error = execute(&mut api, &config(), "old", "new", true, true).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("1 source-label assignments remain")
        );
        api.done();
    }

    #[test]
    fn resolves_label_identity_and_rejects_incomplete_destination_response() {
        let mut api = Script::new();
        resolve(&mut api, "same-id", "same-id");
        assert!(
            plan(&mut api, &config(), "old", "new")
                .unwrap_err()
                .to_string()
                .contains("same label")
        );
        api.done();
        let mut api = Script::new();
        api.expect(
            "query ResolveLabels",
            json!({}),
            json!({"repository":{"old":{"id":"old", "name":"old"}}}),
        );
        assert!(
            plan(&mut api, &config(), "old", "new")
                .unwrap_err()
                .to_string()
                .contains("Missing destination lookup")
        );
        api.done();
    }

    fn resolve_rename(api: &mut Script) {
        api.expect("query ResolveLabels", json!({"old": "old", "new": "new"}),
            json!({"repository": {"old": {"id": "old-id", "name": "old"}, "new": null, "hasDiscussionsEnabled": true}}));
    }

    #[test]
    fn missing_destination_previews_rename_without_writes() {
        let mut api = Script::new();
        resolve_rename(&mut api);
        inventory(
            &mut api,
            json!([item("a", "Issue", &["old-id"])]),
            json!([]),
            json!([]),
        );
        execute(&mut api, &config(), "old", "new", false, false).unwrap();
        api.done();
    }

    #[test]
    fn rename_preserves_identity_without_changing_item_assignments() {
        let mut api = Script::new();
        resolve_rename(&mut api);
        inventory(
            &mut api,
            json!([item("a", "Issue", &["old-id"])]),
            json!([]),
            json!([]),
        );
        resolve_rename(&mut api); // Recheck after confirmation.
        api.expect(
            "mutation RenameLabel",
            json!({"id":"old-id", "name":"new"}),
            json!({"updateLabel":{"label":{"id":"old-id", "name":"new"}}}),
        );
        api.expect(
            "query ResolveLabels",
            json!({}),
            json!({"repository":{"old":null, "new":{"id":"old-id", "name":"new"}}}),
        );
        execute(&mut api, &config(), "old", "new", true, true).unwrap();
        api.done();
    }

    #[test]
    fn rename_aborts_if_destination_appears_after_preview() {
        let mut api = Script::new();
        resolve(&mut api, "old-id", "other-id");
        let old = Label {
            id: "old-id".into(),
            name: "old".into(),
        };
        assert!(
            rename_label(&mut api, &config(), &old, "new")
                .unwrap_err()
                .to_string()
                .contains("Destination appeared")
        );
        api.done();
    }

    #[test]
    fn rename_aborts_if_source_is_replaced_after_preview() {
        let mut api = Script::new();
        resolve_rename(&mut api);
        let old = Label {
            id: "different-id".into(),
            name: "old".into(),
        };
        assert!(
            rename_label(&mut api, &config(), &old, "new")
                .unwrap_err()
                .to_string()
                .contains("Source label changed")
        );
        api.done();
    }

    #[test]
    fn rename_requires_verified_final_identity() {
        let mut api = Script::new();
        resolve_rename(&mut api);
        api.expect(
            "mutation RenameLabel",
            json!({"id":"old-id", "name":"new"}),
            json!({"updateLabel":{"label":{"id":"old-id", "name":"new"}}}),
        );
        api.expect(
            "query ResolveLabels",
            json!({}),
            json!({"repository":{"old":null, "new":{"id":"different-id", "name":"new"}}}),
        );
        let old = Label {
            id: "old-id".into(),
            name: "old".into(),
        };
        assert!(
            rename_label(&mut api, &config(), &old, "new")
                .unwrap_err()
                .to_string()
                .contains("could not be verified")
        );
        api.done();
    }

    #[test]
    fn partial_graphql_errors_and_missing_data_are_failures() {
        assert!(decode_response(json!({"data":{"change":{"labelable":{"id":"a"}}}, "errors":[{"message":"rate limited"}]})).is_err());
        assert!(decode_response(json!({"data":null})).is_err());
        assert!(decode_response(json!({"data":{},"errors":[]})).is_ok());
    }

    #[test]
    fn missing_or_cyclic_cursors_are_errors() {
        let mut seen = HashSet::new();
        assert!(
            next_cursor(
                PageInfo {
                    has_next_page: true,
                    end_cursor: None
                },
                &mut seen
            )
            .is_err()
        );
        assert_eq!(
            next_cursor(
                PageInfo {
                    has_next_page: true,
                    end_cursor: Some("c".into())
                },
                &mut seen
            )
            .unwrap(),
            Some("c".into())
        );
        assert!(
            next_cursor(
                PageInfo {
                    has_next_page: true,
                    end_cursor: Some("c".into())
                },
                &mut seen
            )
            .is_err()
        );
    }
}
