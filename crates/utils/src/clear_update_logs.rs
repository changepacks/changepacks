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
                        let expected =
                            closers.pop().context("unexpected JSON closing delimiter")?;
                        if bytes[cursor] != expected {
                            bail!("mismatched JSON closing delimiter at byte {cursor}");
                        }
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
    //   Phase 3: the existing sequential parse+retain+remove-or-rewrite loop is
    //            unchanged — it must remain sequential because each file may be
    //            removed or rewritten depending on the `applied_paths` set.
    let paths = collect_changepack_log_paths(changepacks_dir).await?;
    let bodies = read_log_bodies(&paths, "update log").await?;
    for (path, content) in paths.iter().zip(bodies) {
        let mut value: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse update log {}", path.display()))?;

        let Some(changes) = value
            .get_mut("changes")
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };

        changes.retain(|change_path, _| !applied_paths.contains(std::path::Path::new(change_path)));
        if changes.is_empty() {
            remove_file(path)
                .await
                .with_context(|| format!("Failed to remove update log {}", path.display()))?;
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
    use crate::get_changepacks_dir;

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_clear_update_logs_empty_directory() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create .changepacks directory
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Test clearing logs when .changepacks directory doesn't exist
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_clear_update_logs_with_config_json_only() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create .changepacks directory
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create .changepacks directory
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create .changepacks directory
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create .changepacks directory
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create .changepacks directory
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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
    async fn test_clear_applied_update_logs_rewrites_mixed_log_preserving_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
        let applied_paths = HashSet::from([PathBuf::from("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn clear_applied_update_logs_preserves_key_order() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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

        crate::test_support::init_git_repo(temp_path);

        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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
        crate::test_support::init_git_repo(temp_path);
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
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
}
