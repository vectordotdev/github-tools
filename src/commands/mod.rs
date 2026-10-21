use serde::Serialize;

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

/// Serialize snapshots with alphabetical object keys at every depth.
pub(super) fn sorted_json(value: &impl Serialize) -> serde_json::Result<String> {
    let mut value = serde_json::to_value(value)?;
    value.sort_all_objects();
    serde_json::to_string_pretty(&value)
}

#[cfg(test)]
mod tests {
    use super::sorted_json;
    use serde::Serialize;
    use serde_json::{Value, json};

    #[test]
    fn sorts_nested_objects_without_changing_values_or_array_order() {
        let value: Value = serde_json::from_str(
            r#"{"z":null,"items":[{"z":2,"a":1},{"id":1},3,2],"a":{"z":false,"a":"text"}}"#,
        )
        .unwrap();
        let saved = sorted_json(&value).unwrap();
        let expected: Value = serde_json::from_str(
            r#"{"a":{"a":"text","z":false},"items":[{"a":1,"z":2},{"id":1},3,2],"z":null}"#,
        )
        .unwrap();
        assert_eq!(saved, serde_json::to_string_pretty(&expected).unwrap());
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), value);
    }

    #[test]
    fn typed_snapshots_use_the_same_order_as_json_objects() {
        #[derive(Serialize)]
        struct Snapshot {
            z: u64,
            a: Value,
        }
        let typed = Snapshot {
            z: u64::MAX,
            a: json!({"z": 2, "a": 1}),
        };
        let expected: Value =
            serde_json::from_str(r#"{"a":{"a":1,"z":2},"z":18446744073709551615}"#).unwrap();
        assert_eq!(
            sorted_json(&typed).unwrap(),
            serde_json::to_string_pretty(&expected).unwrap()
        );
    }

    #[test]
    fn response_key_order_and_repeated_saves_do_not_change_bytes() {
        let responses = [
            r#"{"title":"same","labels":[{"name":"bug","id":1}],"number":1}"#,
            r#"{"number":1,"labels":[{"id":1,"name":"bug"}],"title":"same"}"#,
        ];
        let expected = sorted_json(&serde_json::from_str::<Value>(responses[0]).unwrap()).unwrap();
        for response in responses {
            let mut value: Value = serde_json::from_str(response).unwrap();
            for _ in 0..3 {
                let saved = sorted_json(&value).unwrap();
                assert_eq!(saved, expected);
                value = serde_json::from_str(&saved).unwrap();
            }
        }
    }
}
