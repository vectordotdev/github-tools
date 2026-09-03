use serde_json::Value;
use std::collections::HashMap;

pub mod build_db;
pub mod compact;
pub mod close_old_prs;
pub mod generate_charts;
pub mod delete_stale_branches;
pub mod fetch_automated_review_stats;
pub mod fetch_discussions;
pub mod fetch_issues;
pub mod fetch_labels;
pub mod generate_summaries;
pub mod purge;
pub mod push_metrics;
pub mod remove_legacy_label;
pub mod workflows;

/// Replace an existing record only when its content changed.
///
/// With serde_json's `preserve_order` feature, retaining an equal existing value
/// also retains its serialized key order and avoids formatting-only data diffs.
pub(super) fn upsert_json_record(
    records: &mut HashMap<u64, Value>,
    number: u64,
    fresh: Value,
) {
    if records.get(&number) != Some(&fresh) {
        records.insert(number, fresh);
    }
}

#[cfg(test)]
mod tests {
    use super::upsert_json_record;
    use serde_json::{json, Value};
    use std::collections::HashMap;

    #[test]
    fn preserves_existing_key_order_for_equal_records() {
        let existing: Value = serde_json::from_str(r#"{"title":"same","number":1}"#).unwrap();
        let mut records = HashMap::from([(1, existing)]);

        upsert_json_record(&mut records, 1, json!({"number": 1, "title": "same"}));

        let keys: Vec<_> = records[&1].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["title", "number"]);
    }

    #[test]
    fn replaces_records_when_content_changed() {
        let mut records = HashMap::from([(1, json!({"number": 1, "title": "old"}))]);

        upsert_json_record(&mut records, 1, json!({"number": 1, "title": "new"}));

        assert_eq!(records[&1]["title"], "new");
    }
}
