//! Canonical, atomic writes for complete repository snapshots.
use super::sorted_json;
use anyhow::{Context, Result};
use serde::Serialize;
use std::{fs, path::Path};

/// Serialize completely before replacing a snapshot in the same directory.
pub(super) fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("Missing output directory")?)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, sorted_json(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub(super) fn save_latest(directory: &Path, value: &impl Serialize) -> Result<()> {
    save(&directory.join("latest.json"), value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::graphql_fetch::test_support::TempDir;
    use serde::Serializer;
    use serde_json::json;

    #[test]
    fn creates_directories_and_replaces_latest_with_canonical_json() {
        let temporary = TempDir::new();
        let directory = temporary.path().join("snapshot");
        save_latest(&directory, &json!({"z": 1, "a": {"z": 2, "a": 3}})).unwrap();
        assert_eq!(
            fs::read_to_string(directory.join("latest.json")).unwrap(),
            "{\n  \"a\": {\n    \"a\": 3,\n    \"z\": 2\n  },\n  \"z\": 1\n}"
        );
        save_latest(&directory, &json!({"new": true})).unwrap();
        assert_eq!(
            fs::read_to_string(directory.join("latest.json")).unwrap(),
            "{\n  \"new\": true\n}"
        );
        assert!(!directory.join("latest.json.tmp").exists());
    }

    #[test]
    fn serialization_failure_preserves_the_previous_snapshot() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: Serializer>(&self, _: S) -> std::result::Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("fixture serialization failure"))
            }
        }
        let temporary = TempDir::new();
        let path = temporary.path().join("metadata.json");
        fs::write(&path, "previous complete snapshot").unwrap();
        assert!(save(&path, &Invalid).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "previous complete snapshot"
        );
        assert!(!path.with_extension("json.tmp").exists());
    }
}
