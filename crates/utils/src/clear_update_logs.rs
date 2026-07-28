use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tokio::fs::{remove_file, write};

use crate::{collect_changepack_log_paths, read_log_bodies};

struct JsonObjectMember {
    prefix_start: usize,
    key: String,
    value_start: usize,
    value_end: usize,
    comma: Option<usize>,
}

fn skip_json_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'\t' | b'\r' | b'\n') {
        cursor += 1;
    }
    cursor
}

fn scan_json_string_end(bytes: &[u8], start: usize) -> Result<usize> {
    if bytes.get(start) != Some(&b'"') {
        bail!("expected JSON string at byte {start}");
    }

    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            b'"' => return Ok(cursor + 1),
            _ => cursor += 1,
        }
    }
    bail!("unterminated JSON string at byte {start}")
}

fn scan_json_value_end(bytes: &[u8], start: usize) -> Result<usize> {
    let start = skip_json_whitespace(bytes, start);
    match bytes.get(start) {
        Some(b'"') => scan_json_string_end(bytes, start),
        Some(b'{') | Some(b'[') => {
            let mut closers = vec![if bytes[start] == b'{' { b'}' } else { b']' }];
            let mut cursor = start + 1;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'"' => cursor = scan_json_string_end(bytes, cursor)?,
                    b'{' => {
                        closers.push(b'}');
                        cursor += 1;
                    }
                    b'[' => {
                        closers.push(b']');
                        cursor += 1;
                    }
                    b'}' | b']' => {
                        if closers.last() != Some(&bytes[cursor]) {
                            bail!("mismatched JSON closing delimiter at byte {cursor}");
                        }
                        closers.pop();
                        cursor += 1;
                        if closers.is_empty() {
                            return Ok(cursor);
                        }
                    }
                    _ => cursor += 1,
                }
            }
            bail!("unterminated JSON value at byte {start}")
        }
        Some(_) => {
            let mut cursor = start;
            while cursor < bytes.len()
                && !matches!(
                    bytes[cursor],
                    b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']'
                )
            {
                cursor += 1;
            }
            Ok(cursor)
        }
        None => bail!("expected JSON value at end of input"),
    }
}

fn parse_json_object_members(content: &str, open: usize) -> Result<Vec<JsonObjectMember>> {
    let bytes = content.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        bail!("expected JSON object at byte {open}");
    }

    let mut members = Vec::new();
    let mut cursor = open + 1;
    loop {
        let prefix_start = cursor;
        let key_start = skip_json_whitespace(bytes, cursor);
        if bytes.get(key_start) == Some(&b'}') {
            return Ok(members);
        }

        let key_end = scan_json_string_end(bytes, key_start)?;
        let key: String = serde_json::from_str(&content[key_start..key_end])?;
        cursor = skip_json_whitespace(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            bail!("expected ':' after JSON object key at byte {cursor}");
        }

        let value_start = skip_json_whitespace(bytes, cursor + 1);
        let value_end = scan_json_value_end(bytes, value_start)?;
        cursor = skip_json_whitespace(bytes, value_end);
        let comma = if bytes.get(cursor) == Some(&b',') {
            let comma = cursor;
            cursor += 1;
            Some(comma)
        } else if bytes.get(cursor) == Some(&b'}') {
            None
        } else {
            bail!("expected ',' or '}}' after JSON object member at byte {cursor}");
        };

        members.push(JsonObjectMember {
            prefix_start,
            key,
            value_start,
            value_end,
            comma,
        });
        if comma.is_none() {
            return Ok(members);
        }
    }
}

fn remove_applied_change_spans(content: &str, applied_paths: &HashSet<PathBuf>) -> Result<String> {
    let root_open = skip_json_whitespace(content.as_bytes(), 0);
    let root_members = parse_json_object_members(content, root_open)?;
    let changes = root_members
        .iter()
        .rev()
        .find(|member| member.key == "changes")
        .context("parsed update log is missing its changes object")?;
    let members = parse_json_object_members(content, changes.value_start)?;
    let selected: Vec<bool> = members
        .iter()
        .map(|member| applied_paths.contains(Path::new(&member.key)))
        .collect();

    let mut removals = Vec::new();
    let mut cursor = 0;
    while cursor < members.len() {
        if !selected[cursor] {
            cursor += 1;
            continue;
        }

        let run_start = cursor;
        while cursor < members.len() && selected[cursor] {
            cursor += 1;
        }
        let run_end = cursor;
        if run_end < members.len() {
            let comma = members[run_end - 1]
                .comma
                .context("selected non-final JSON member is missing its comma")?;
            removals.push((members[run_start].prefix_start, comma + 1));
        } else if run_start == 0 {
            removals.push((members[0].prefix_start, members[run_end - 1].value_end));
        } else {
            let previous_comma = members[run_start - 1]
                .comma
                .context("JSON member before selected final run is missing its comma")?;
            removals.push((previous_comma, members[run_end - 1].value_end));
        }
    }

    let removed_len: usize = removals.iter().map(|(start, end)| end - start).sum();
    let mut output = String::with_capacity(content.len() - removed_len);
    let mut copied_through = 0;
    for (start, end) in removals {
        output.push_str(&content[copied_through..start]);
        copied_through = end;
    }
    output.push_str(&content[copied_through..]);
    Ok(output)
}

/// Remove all update logs without confirmation
///
/// Uses [`collect_changepack_log_paths`] — which applies the same predicate
/// [`gen_update_map`](crate::gen_update_map) uses — so the cleaner and the
/// reader stay in lock-step. A file the reader intentionally ignores
/// (`.gitkeep`, `README.md`, etc.) is not silently destroyed the first time
/// `update` completes.
///
/// # Errors
/// Returns error if any update log file fails to be removed.
pub async fn clear_update_logs(changepacks_dir: &Path) -> Result<()> {
    // Two-phase collect+delete, mirroring `gen_update_map`:
    //   Phase 1: single directory walk to collect the paths of every matching
    //            `changepack_log_*.json` entry — pure name filtering, no IO body.
    //   Phase 2: map the collected paths into `remove_file` futures and hand
    //            the exact-size iterator to `futures::future::join_all`.
    //   Keeping the reader (`gen_update_map`) and cleaner (`clear_update_logs`)
    //   in lock-step on the shape reinforces the shared-predicate invariant.
    let paths = collect_changepack_log_paths(changepacks_dir).await?;
    let results = futures::future::join_all(paths.iter().map(remove_file)).await;
    let mut error_details = Vec::new();
    for (path, result) in paths.iter().zip(results) {
        if let Err(err) = result {
            error_details.push(format!("{}: {err}", path.display()));
        }
    }
    if error_details.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Failed to remove {} update log(s): {}",
            error_details.len(),
            error_details.join("; ")
        ))
    }
}

/// Remove or rewrite update logs after a selective update.
///
/// Fully applied changepack logs are deleted. Mixed logs are rewritten with
/// only unapplied `changes` entries while preserving sibling fields such as
/// `note` and `date`.
///
/// # Errors
/// Returns error if a matching changepack log cannot be read, parsed, removed,
/// or rewritten.
pub async fn clear_applied_update_logs(
    changepacks_dir: &Path,
    applied_paths: &HashSet<PathBuf>,
) -> Result<()> {
    // Two-phase read, mirroring `gen_update_map`:
    //   Phase 1: single directory walk to collect the paths of every matching
    //            `changepack_log_*.json` entry — pure name filtering, no IO body.
    //   Phase 2: the shared `read_log_bodies` helper reads every body
    //            concurrently via `try_join_all`, collapsing N sequential
    //            `read_to_string` round-trips into one parallel batch on
    //            IO-bound systems.
    //   Phase 3: the sequential parse+classify+remove-or-rewrite loop remains
    //            ordered because each file may be removed, rewritten, or left
    //            untouched depending on the `applied_paths` set.
    //   The `serde_json::Value` parse below is a read-only CLASSIFIER: it only
    //   counts how many `changes` entries survive so the branch can pick
    //   remove / skip / rewrite. It is never the rewriter — the byte-preserving
    //   output comes from `remove_applied_change_spans` operating on the
    //   ORIGINAL content string, which is the invariant that protects
    //   formatting (key order, indentation, trailing newline).
    let paths = collect_changepack_log_paths(changepacks_dir).await?;
    let bodies = read_log_bodies(&paths, "update log").await?;
    for (path, content) in paths.iter().zip(bodies) {
        let value: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse update log {}", path.display()))?;

        let Some(changes) = value.get("changes").and_then(serde_json::Value::as_object) else {
            continue;
        };

        let before = changes.len();
        let remaining = changes
            .keys()
            .filter(|change_path| !applied_paths.contains(Path::new(change_path.as_str())))
            .count();
        if remaining == 0 {
            remove_file(path)
                .await
                .with_context(|| format!("Failed to remove update log {}", path.display()))?;
        } else if remaining == before {
            continue;
        } else {
            let next_content = remove_applied_change_spans(&content, applied_paths)
                .with_context(|| format!("Failed to rewrite update log {}", path.display()))?;
            write(path, next_content)
                .await
                .with_context(|| format!("Failed to rewrite update log {}", path.display()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_clear_update_logs_empty_directory() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Test clearing logs from empty directory
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_clear_update_logs_no_changepacks_directory() {
        // Create a temporary directory without .changepacks
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Test clearing logs when .changepacks directory doesn't exist
        let changepacks_dir = temp_path.join(".changepacks");
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_clear_update_logs_with_config_json_only() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Create only config.json
        let config_file = changepacks_dir.join("config.json");
        fs::write(&config_file, r#"{"ignore": [], "baseBranch": "main"}"#).unwrap();

        // Test clearing logs - config.json should remain
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());
        assert!(config_file.exists(), "config.json should not be deleted");
    }

    #[tokio::test]
    async fn test_clear_update_logs_with_update_logs() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Create config.json
        let config_file = changepacks_dir.join("config.json");
        fs::write(&config_file, r#"{"ignore": [], "baseBranch": "main"}"#).unwrap();

        // Create update log files
        let log_file1 = changepacks_dir.join("changepack_log_1.json");
        let log_file2 = changepacks_dir.join("changepack_log_2.json");
        let log_file3 = changepacks_dir.join("changepack_log_3.json");
        fs::write(&log_file1, r#"{"changes": {}, "note": "test1"}"#).unwrap();
        fs::write(&log_file2, r#"{"changes": {}, "note": "test2"}"#).unwrap();
        fs::write(&log_file3, r#"{"changes": {}, "note": "test3"}"#).unwrap();

        // Test clearing logs
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());

        // config.json should remain
        assert!(config_file.exists(), "config.json should not be deleted");

        // All update log files should be deleted
        assert!(
            !log_file1.exists(),
            "changepack_log_1.json should be deleted"
        );
        assert!(
            !log_file2.exists(),
            "changepack_log_2.json should be deleted"
        );
        assert!(
            !log_file3.exists(),
            "changepack_log_3.json should be deleted"
        );
    }

    #[tokio::test]
    async fn test_clear_update_logs_with_mixed_files() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Create config.json
        let config_file = changepacks_dir.join("config.json");
        fs::write(&config_file, r#"{"ignore": [], "baseBranch": "main"}"#).unwrap();

        // Create arbitrary JSON files with names that are not changepack logs.
        let log_file1 = changepacks_dir.join("2024-01-01.json");
        let log_file2 = changepacks_dir.join("2024-01-02.json");
        let log_file3 = changepacks_dir.join("update.json");
        let log_file4 = changepacks_dir.join("log.json");
        fs::write(&log_file1, r#"{"changes": {}, "note": "test1"}"#).unwrap();
        fs::write(&log_file2, r#"{"changes": {}, "note": "test2"}"#).unwrap();
        fs::write(&log_file3, r#"{"changes": {}, "note": "test3"}"#).unwrap();
        fs::write(&log_file4, r#"{"changes": {}, "note": "test4"}"#).unwrap();

        // Test clearing logs
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());

        // config.json should remain
        assert!(config_file.exists(), "config.json should not be deleted");

        // Arbitrary JSON files should be preserved.
        assert!(log_file1.exists(), "2024-01-01.json should be preserved");
        assert!(log_file2.exists(), "2024-01-02.json should be preserved");
        assert!(log_file3.exists(), "update.json should be preserved");
        assert!(log_file4.exists(), "log.json should be preserved");
    }

    #[tokio::test]
    async fn test_clear_update_logs_without_config_json() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Create update log files without config.json
        let log_file1 = changepacks_dir.join("changepack_log_1.json");
        let log_file2 = changepacks_dir.join("changepack_log_2.json");
        fs::write(&log_file1, r#"{"changes": {}, "note": "test1"}"#).unwrap();
        fs::write(&log_file2, r#"{"changes": {}, "note": "test2"}"#).unwrap();

        // Test clearing logs
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());

        // All update log files should be deleted
        assert!(
            !log_file1.exists(),
            "changepack_log_1.json should be deleted"
        );
        assert!(
            !log_file2.exists(),
            "changepack_log_2.json should be deleted"
        );
    }

    #[tokio::test]
    async fn test_clear_update_logs_preserves_non_json_files() {
        // Regression: the cleaner used to delete every entry that was not
        // `config.json`, so user-owned files like `.gitkeep`, `.gitignore`
        // or `README.md` under `.changepacks/` disappeared the first time
        // `update` completed. The sibling `gen_update_map` reader already
        // ignores non-JSON files, so the cleaner MUST mirror that filter.
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Non-JSON files the user could have placed there.
        let gitkeep = changepacks_dir.join(".gitkeep");
        let readme = changepacks_dir.join("README.md");
        fs::write(&gitkeep, "").unwrap();
        fs::write(&readme, "notes about changepacks").unwrap();

        // A legitimate JSON update log that SHOULD be deleted.
        let log_file = changepacks_dir.join("changepack_log_1.json");
        fs::write(&log_file, r#"{"changes": {}, "note": "test"}"#).unwrap();

        // config.json is always preserved.
        let config_file = changepacks_dir.join("config.json");
        fs::write(&config_file, r#"{"ignore": [], "baseBranch": "main"}"#).unwrap();

        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());

        // Non-JSON user files must SURVIVE.
        assert!(gitkeep.exists(), ".gitkeep must not be deleted");
        assert!(readme.exists(), "README.md must not be deleted");
        // config.json must SURVIVE.
        assert!(config_file.exists(), "config.json must not be deleted");
        // JSON update log must be DELETED.
        assert!(!log_file.exists(), "changepack_log_1.json must be deleted");
    }

    #[tokio::test]
    async fn test_full_and_selective_cleanup_preserve_notes_json_bytes() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let notes_file = changepacks_dir.join("notes.json");
        let notes_bytes = b"{\n  \"changes\": {\"user/notes\": \"Major\"},\n  \"note\": \"keep byte-for-byte\"\n}\n";
        fs::write(&notes_file, notes_bytes).unwrap();

        let full_log = changepacks_dir.join("changepack_log_full.json");
        fs::write(&full_log, r#"{"changes":{},"note":"full"}"#).unwrap();
        clear_update_logs(&changepacks_dir).await.unwrap();
        assert!(!full_log.exists());
        assert_eq!(fs::read(&notes_file).unwrap(), notes_bytes);

        let selective_log = changepacks_dir.join("changepack_log_selective.JSON");
        fs::write(
            &selective_log,
            r#"{"changes":{"packages/a/package.json":"Patch"},"note":"selective"}"#,
        )
        .unwrap();
        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        clear_applied_update_logs(&changepacks_dir, &applied_paths)
            .await
            .unwrap();
        assert!(!selective_log.exists());
        assert_eq!(fs::read(&notes_file).unwrap(), notes_bytes);
    }

    #[tokio::test]
    async fn test_clear_update_logs_ignores_json_directory() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create .changepacks directory
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Create a subdirectory with a name that looks like a JSON file.
        let log_dir = changepacks_dir.join("update_log.json");
        fs::create_dir_all(&log_dir).unwrap();

        // Directories are not changepack logs, so clearing succeeds and preserves it.
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());
        assert!(log_dir.is_dir(), "JSON-named directory must be preserved");
    }

    #[tokio::test]
    async fn test_clear_applied_update_logs_deletes_fully_applied_log() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let log_file = changepacks_dir.join("changepack_log_1.json");
        fs::write(
            &log_file,
            r#"{"changes":{"packages/a/package.json":"Patch"},"note":"done","date":"2026-01-01"}"#,
        )
        .unwrap();

        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok());
        assert!(!log_file.exists(), "fully applied log should be deleted");
    }

    #[tokio::test]
    async fn clear_applied_update_logs_skips_untouched_readonly_log() {
        // Given an applied log and a byte-sensitive log with no applied paths.
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let applied_log = changepacks_dir.join("changepack_log_applied.json");
        fs::write(
            &applied_log,
            r#"{"changes":{"packages/a/package.json":"Patch"},"note":"done"}"#,
        )
        .unwrap();

        let untouched_log = changepacks_dir.join("changepack_log_untouched.json");
        let untouched_bytes =
            b"{\n  \"changes\": {\"packages/b/package.json\": \"Minor\"},\n  \"note\": \"keep byte-for-byte\"\n}\n";
        fs::write(&untouched_log, untouched_bytes).unwrap();
        crate::test_support::set_readonly(&untouched_log, true);

        // When selective cleanup applies only the first log.
        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;
        crate::test_support::set_readonly(&untouched_log, false);

        // Then cleanup succeeds without rewriting the untouched log.
        assert!(result.is_ok(), "selective cleanup failed: {result:?}");
        assert!(!applied_log.exists(), "applied log should be deleted");
        assert_eq!(fs::read(untouched_log).unwrap(), untouched_bytes);
    }

    #[tokio::test]
    async fn test_clear_applied_update_logs_rewrites_mixed_log_preserving_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let log_file = changepacks_dir.join("changepack_log_1.json");
        fs::write(
            &log_file,
            r#"{"changes":{"packages/a/package.json":"Patch","packages/b/package.json":"Minor"},"note":"keep this","date":"2026-01-01"}"#,
        )
        .unwrap();

        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok());
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&log_file).unwrap()).unwrap();
        assert_eq!(value["changes"].as_object().unwrap().len(), 1);
        assert_eq!(value["changes"]["packages/b/package.json"], "Minor");
        assert!(value["changes"].get("packages/a/package.json").is_none());
        assert_eq!(value["note"], "keep this");
        assert_eq!(value["date"], "2026-01-01");
    }

    #[tokio::test]
    async fn test_clear_applied_update_logs_ignores_config_and_non_json_files() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let config_file = changepacks_dir.join("config.json");
        let readme = changepacks_dir.join("README.md");
        fs::write(&config_file, r#"{"ignore":[],"baseBranch":"main"}"#).unwrap();
        fs::write(&readme, "notes").unwrap();

        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok());
        assert!(config_file.exists(), "config.json should be ignored");
        assert!(readme.exists(), "non-json file should be ignored");
    }

    #[tokio::test]
    async fn test_clear_applied_update_logs_missing_directory_returns_ok() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let changepacks_dir = temp_path.join(".changepacks");
        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok());
    }

    /// Write `bytes` as a changepack log, run selective cleanup with a
    /// non-empty applied set, and assert the file survived byte-identical.
    async fn assert_selective_clear_leaves_log_untouched(bytes: &[u8]) {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let log_file = changepacks_dir.join("changepack_log_1.json");
        fs::write(&log_file, bytes).unwrap();

        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok(), "selective cleanup failed: {result:?}");
        assert!(
            log_file.exists(),
            "log without a changes object must not be deleted"
        );
        assert_eq!(
            fs::read(&log_file).unwrap(),
            bytes,
            "log without a changes object must stay byte-identical"
        );
    }

    #[tokio::test]
    async fn clear_applied_update_logs_leaves_log_without_changes_key_untouched() {
        // A hand-edited or future-schema log that carries no `changes` key at
        // all must be left alone: the cleaner only owns logs it can classify.
        assert_selective_clear_leaves_log_untouched(
            b"{\n  \"note\": \"hand written\",\n  \"date\": \"2026-01-01\"\n}\n",
        )
        .await;
    }

    #[tokio::test]
    async fn clear_applied_update_logs_leaves_log_with_non_object_changes_untouched() {
        // Same protection when `changes` exists but is not a JSON object
        // (here an array): it is not the schema the cleaner understands, so
        // the file must survive byte-identical rather than be deleted.
        assert_selective_clear_leaves_log_untouched(
            b"{\n  \"changes\": [],\n  \"note\": \"array schema\",\n  \"date\": \"2026-01-01\"\n}\n",
        )
        .await;
    }

    #[tokio::test]
    async fn clear_applied_update_logs_preserves_key_order() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Create a changepack log with keys in order: changes, note, date
        // (note BEFORE date to test order preservation)
        let log_file = changepacks_dir.join("changepack_log_1.json");
        fs::write(
            &log_file,
            r#"{"changes":{"packages/a/package.json":"Patch","packages/b/package.json":"Minor"},"note":"test note","date":"2026-01-01"}"#,
        )
        .unwrap();

        // Apply only one change, so the file gets rewritten (not deleted)
        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok());
        assert!(
            log_file.exists(),
            "log file should be rewritten, not deleted"
        );

        // Read the rewritten file as a raw string
        let content = fs::read_to_string(&log_file).unwrap();

        // Assert that key order is preserved: "note" appears before "date"
        let note_pos = content.find("\"note\"").expect("\"note\" key should exist");
        let date_pos = content.find("\"date\"").expect("\"date\" key should exist");
        assert!(
            note_pos < date_pos,
            "key order not preserved: \"note\" at {} should come before \"date\" at {}",
            note_pos,
            date_pos
        );
    }

    async fn assert_selective_clear_preserves_formatting(input: &str, expected: &str) {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let log_file = changepacks_dir.join("changepack_log_1.json");
        fs::write(&log_file, input.as_bytes()).unwrap();

        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        clear_applied_update_logs(&changepacks_dir, &applied_paths)
            .await
            .unwrap();

        assert_eq!(fs::read(log_file).unwrap(), expected.as_bytes());
    }

    #[tokio::test]
    async fn clear_applied_update_logs_preserves_tab_indentation() {
        let input = "{\n\t\"changes\": {\n\t\t\"packages/a/package.json\": \"Patch\",\n\t\t\"packages/b/package.json\": \"Minor\"\n\t},\n\t\"note\": \"keep\"\n}";
        let expected = "{\n\t\"changes\": {\n\t\t\"packages/b/package.json\": \"Minor\"\n\t},\n\t\"note\": \"keep\"\n}";

        assert_selective_clear_preserves_formatting(input, expected).await;
    }

    #[tokio::test]
    async fn clear_applied_update_logs_preserves_four_space_indentation() {
        let input = "{\n    \"changes\": {\n        \"packages/a/package.json\": \"Patch\",\n        \"packages/b/package.json\": \"Minor\"\n    },\n    \"note\": \"keep\"\n}";
        let expected = "{\n    \"changes\": {\n        \"packages/b/package.json\": \"Minor\"\n    },\n    \"note\": \"keep\"\n}";

        assert_selective_clear_preserves_formatting(input, expected).await;
    }

    #[tokio::test]
    async fn clear_applied_update_logs_preserves_final_newline() {
        let input = "{\n  \"changes\": {\n    \"packages/a/package.json\": \"Patch\",\n    \"packages/b/package.json\": \"Minor\"\n  },\n  \"note\": \"keep\"\n}\n";
        let expected = "{\n  \"changes\": {\n    \"packages/b/package.json\": \"Minor\"\n  },\n  \"note\": \"keep\"\n}\n";

        assert_selective_clear_preserves_formatting(input, expected).await;
    }

    #[tokio::test]
    async fn clear_applied_update_logs_preserves_no_final_newline() {
        let input = "{\n  \"changes\": {\n    \"packages/a/package.json\": \"Patch\",\n    \"packages/b/package.json\": \"Minor\"\n  },\n  \"note\": \"keep\"\n}";
        let expected = "{\n  \"changes\": {\n    \"packages/b/package.json\": \"Minor\"\n  },\n  \"note\": \"keep\"\n}";

        assert_selective_clear_preserves_formatting(input, expected).await;
    }

    #[tokio::test]
    async fn clear_applied_update_logs_preserves_compact_json_bytes() {
        let input = r#"{"changes":{"packages/a/package.json":"Patch","packages/b/package.json":"Minor, \"quoted\""},"note":"keep"}"#;
        let expected =
            r#"{"changes":{"packages/b/package.json":"Minor, \"quoted\""},"note":"keep"}"#;

        assert_selective_clear_preserves_formatting(input, expected).await;
    }

    #[tokio::test]
    async fn clear_applied_update_logs_preserves_irregular_json_bytes() {
        let input = "{\n \"changes\" : {  \"packages/a/package.json\" : \"Patch\" ,\t\"packages/b/package.json\": [1,{\"text\":\"comma, and \\\"quote\\\"\"}]   }, \"note\"  :true\n}\n";
        let expected = "{\n \"changes\" : {\t\"packages/b/package.json\": [1,{\"text\":\"comma, and \\\"quote\\\"\"}]   }, \"note\"  :true\n}\n";

        assert_selective_clear_preserves_formatting(input, expected).await;
    }

    #[tokio::test]
    async fn clear_applied_update_logs_removes_multiple_selected_entries() {
        let input = r#"{"changes":{"packages/a/package.json":"Patch, one","packages/c/package.json":"Major, two","packages/b/package.json":"Minor, \"keep\""},"note":"keep"}"#;
        let expected = r#"{"changes":{"packages/b/package.json":"Minor, \"keep\""},"note":"keep"}"#;

        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        let changepacks_dir = temp_path.join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();
        let log_file = changepacks_dir.join("changepack_log_1.json");
        fs::write(&log_file, input.as_bytes()).unwrap();

        let applied_paths = HashSet::from([
            PathBuf::from("packages/a/package.json"),
            PathBuf::from("packages/c/package.json"),
        ]);
        clear_applied_update_logs(&changepacks_dir, &applied_paths)
            .await
            .unwrap();

        assert_eq!(fs::read(log_file).unwrap(), expected.as_bytes());
    }

    /// Run the byte-preserving rewriter directly on `content` and return the
    /// flattened error chain.
    ///
    /// `clear_applied_update_logs` runs `serde_json::from_str` as a classifier
    /// before it ever reaches the rewriter, so these malformed inputs can only
    /// be driven through the module-private entry point. The guards are what
    /// keeps a hand-edited or future-schema changepack log from being silently
    /// rewritten into corrupted bytes, so each one is pinned to its message.
    fn scanner_error(content: &str) -> String {
        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let error = remove_applied_change_spans(content, &applied_paths)
            .expect_err("malformed JSON must be rejected instead of rewritten");
        format!("{error:#}")
    }

    #[test]
    fn remove_applied_change_spans_rejects_non_object_root() {
        assert_eq!(
            scanner_error(r#"["packages/a/package.json"]"#),
            "expected JSON object at byte 0"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_unterminated_root_object() {
        // The root brace opens and input ends, so the member loop asks for a
        // key that is not there.
        assert_eq!(scanner_error("{"), "expected JSON string at byte 1");
    }

    #[test]
    fn remove_applied_change_spans_rejects_unquoted_member_key() {
        assert_eq!(
            scanner_error("{changes: {}}"),
            "expected JSON string at byte 1"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_unterminated_string_key() {
        assert_eq!(
            scanner_error(r#"{"changes"#),
            "unterminated JSON string at byte 1"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_invalid_escape_in_key() {
        // `scan_json_string_end` skips the escape pair, then `serde_json`
        // rejects it while decoding the key.
        let error = scanner_error(r#"{"\q":1}"#);
        assert!(
            error.contains("invalid escape"),
            "unexpected error text: {error}"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_missing_colon_after_key() {
        assert_eq!(
            scanner_error(r#"{"changes" 1}"#),
            "expected ':' after JSON object key at byte 11"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_missing_member_separator() {
        assert_eq!(
            scanner_error(r#"{"changes":{} "note":1}"#),
            "expected ',' or '}' after JSON object member at byte 14"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_value_at_end_of_input() {
        assert_eq!(
            scanner_error(r#"{"changes":"#),
            "expected JSON value at end of input"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_unterminated_nested_value() {
        assert_eq!(
            scanner_error(r#"{"changes":[1,2"#),
            "unterminated JSON value at byte 11"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_mismatched_closing_delimiter() {
        assert_eq!(
            scanner_error(r#"{"changes":[1,2}"#),
            "mismatched JSON closing delimiter at byte 15"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_root_without_changes_member() {
        assert_eq!(
            scanner_error(r#"{"note":"hand written","date":"2026-01-01"}"#),
            "parsed update log is missing its changes object"
        );
    }

    #[test]
    fn remove_applied_change_spans_rejects_non_object_changes_value() {
        // `clear_applied_update_logs` filters this shape out up front, but the
        // rewriter must still refuse it rather than mangle the array.
        assert_eq!(
            scanner_error(r#"{"changes":[],"note":"array schema"}"#),
            "expected JSON object at byte 11"
        );
    }
}
