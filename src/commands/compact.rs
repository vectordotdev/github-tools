use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Deduplicate JSON year files in a directory.
/// For issues/PRs: dedup by "id". For discussions: dedup by "number".
pub fn run(dir: &str) -> Result<()> {
    let path = Path::new(dir);
    if !path.is_dir() {
        anyhow::bail!("{dir} is not a directory");
    }

    let mut entries: Vec<_> = fs::read_dir(path)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort_by_key(|e| e.path());

    if entries.is_empty() {
        println!("No JSON files found in {dir}");
        return Ok(());
    }

    // Detect key field from first file
    let first_json = fs::read_to_string(entries[0].path())?;
    let first_items: Vec<Value> = serde_json::from_str(&first_json)?;
    let key_field = if first_items.first().and_then(|v| v.get("number")).is_some()
        && first_items.first().and_then(|v| v.get("id")).is_none()
    {
        "number"
    } else {
        "id"
    };
    println!("Deduplicating by \"{key_field}\" in {dir}");

    let mut total_before = 0;
    let mut total_after = 0;

    for entry in entries {
        let file_path = entry.path();
        let json = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read {}", file_path.display()))?;
        let items: Vec<Value> = serde_json::from_str(&json)?;
        let before = items.len();

        // Dedup: last occurrence wins (preserves most recent state)
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut deduped: Vec<Value> = Vec::new();
        for item in items {
            let key = item[key_field].to_string();
            if let Some(idx) = seen.get(&key) {
                deduped[*idx] = item;
            } else {
                seen.insert(key, deduped.len());
                deduped.push(item);
            }
        }

        let after = deduped.len();
        total_before += before;
        total_after += after;

        if before != after {
            let json_str = serde_json::to_string_pretty(&deduped)?;
            fs::write(&file_path, json_str)?;
            println!(
                "  {}: {} -> {} (removed {} duplicates)",
                file_path.display(),
                before,
                after,
                before - after
            );
        } else {
            println!("  {}: {} items (no duplicates)", file_path.display(), after);
        }
    }

    println!(
        "Total: {} -> {} (removed {} duplicates)",
        total_before,
        total_after,
        total_before - total_after
    );
    Ok(())
}
