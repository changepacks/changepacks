use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use tokio::fs::{remove_file, write};

use crate::{
    applied_change_spans::remove_applied_change_spans, collect_changepack_log_paths,
    read_log_bodies,
};

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
/// `applied_paths` is a set of BORROWED paths: every membership probe below is
/// already a `&Path` lookup, so the caller hands over views into the update map
/// it still owns instead of cloning one `PathBuf` per applied project. Mirrors
/// the borrow-the-keys policy documented in [`gen_update_map`](crate::gen_update_map).
///
/// # Errors
/// Returns error if a matching changepack log cannot be read, parsed, removed,
/// or rewritten.
pub async fn clear_applied_update_logs(
    changepacks_dir: &Path,
    applied_paths: &HashSet<&Path>,
) -> Result<()> {
    // Two-phase read, mirroring `gen_update_map`:
    //   Phase 1: single directory walk to collect the paths of every matching
    //            `changepack_log_*.json` entry — pure name filtering, no IO body.
    //   Phase 2: the shared `read_log_bodies` helper reads every body
    //            concurrently via `try_join_all`, collapsing N sequential
    //            `read_to_string` round-trips into one parallel batch on
    //            IO-bound systems.
    //   Phase 3: the parse+classify loop is CPU-only — it decides
    //            remove / skip / rewrite per log and buffers the decisions,
    //            performing no IO of its own.
    //   Phase 4: the buffered decisions are executed as two OVERLAPPED batches
    //            — `join_all` over the removals and `join_all` over the
    //            rewrites, driven together by one `tokio::join!` — so N
    //            sequential `remove_file`/`write` round-trips collapse into a
    //            single parallel wave, matching the batched read half and the
    //            sibling `clear_update_logs`.
    //   The `serde_json::Value` parse below is a read-only CLASSIFIER: it only
    //   counts how many `changes` entries survive so the branch can pick
    //   remove / skip / rewrite. It is never the rewriter — the byte-preserving
    //   output comes from `remove_applied_change_spans` operating on the
    //   ORIGINAL content string, which is the invariant that protects
    //   formatting (key order, indentation, trailing newline).
    let paths = collect_changepack_log_paths(changepacks_dir).await?;
    let bodies = read_log_bodies(&paths, "update log").await?;
    // The classify loop below walks `paths` exactly once and pushes each path
    // into AT MOST ONE of these two buffers, so `removals.len() + rewrites.len()
    // <= paths.len()` always holds and reserving `paths.len()` for BOTH would
    // reserve 2N slots to serve at most N pushes.
    // `removals` keeps its reservation because it is the only arm that can take
    // every path: a log all of whose `changes` entries were applied is deleted
    // whole, which is what a single-language changepack looks like under the
    // `--language` filter that is the sole caller of this function. The hint
    // removes the ~log2(N) geometric-doubling reallocations there.
    // `rewrites` instead follows the empty-on-the-common-path policy this
    // comment already documents for `error_details` in `clear_update_logs`: it
    // only fires for a log whose `changes` map STRADDLES the applied/unapplied
    // boundary, so the other logs of the same run take the remove or the skip
    // arm and its reservation is waste rather than a saving. It grows on demand.
    let mut removals: Vec<&PathBuf> = Vec::with_capacity(paths.len());
    let mut rewrites: Vec<(&PathBuf, String)> = Vec::new();
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
            removals.push(path);
        } else if remaining == before {
            continue;
        } else {
            let next_content = remove_applied_change_spans(&content, applied_paths)
                .with_context(|| format!("Failed to rewrite update log {}", path.display()))?;
            rewrites.push((path, next_content));
        }
    }

    // The two batches touch PROVABLY DISJOINT paths — the classify loop above
    // walks `paths` once and pushes each entry into AT MOST ONE of `removals`
    // and `rewrites` — and neither reads what the other writes, so there is no
    // data dependency between them and awaiting the removals to completion
    // before issuing the first rewrite was incidental serialization.
    //
    // `tokio::join!` over two `join_all` batches, deliberately NOT
    // `tokio::try_join!`: `try_join!` surfaces whichever branch fails first in
    // WALL-CLOCK time, so a run where both a removal and a rewrite fail would
    // report a different message depending on disk timing. Decoding
    // `removal_results` fully and `?`-ing it BEFORE looking at
    // `rewrite_results` keeps the reported error identical to the previous
    // serial shape. Mirrors the `tokio::join!`-plus-ordered-decode pattern
    // already used in `snapshot_update_state` in the `cli` crate.
    let (removal_results, rewrite_results) = tokio::join!(
        futures::future::join_all(removals.into_iter().map(|path| async move {
            remove_file(path)
                .await
                .with_context(|| format!("Failed to remove update log {}", path.display()))
        })),
        futures::future::join_all(rewrites.into_iter().map(|(path, next_content)| async move {
            write(path, next_content)
                .await
                .with_context(|| format!("Failed to rewrite update log {}", path.display()))
        }))
    );
    removal_results.into_iter().collect::<Result<()>>()?;
    rewrite_results.into_iter().collect::<Result<()>>()?;

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
        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
        clear_applied_update_logs(&changepacks_dir, &applied_paths)
            .await
            .unwrap();
        assert!(!selective_log.exists());
        assert_eq!(fs::read(&notes_file).unwrap(), notes_bytes);
    }

    /// RAII fixture that makes deleting the given changepack logs FAIL, so
    /// [`clear_update_logs`] is forced onto its error-aggregation arm. Dropping
    /// the guard restores normal deletion, so `TempDir` cleanup still succeeds
    /// even when an assertion panics first.
    ///
    /// Unix half: `unlink` is authorized by the WRITE bit of the CONTAINING
    /// DIRECTORY — the file's own mode is irrelevant to it — so the directory
    /// drops to `r-xr-xr-x`. `read_dir` only needs `r-x`, so collection still
    /// sees every log and it is exactly the deletions that are refused.
    #[cfg(unix)]
    struct DenyLogRemoval {
        changepacks_dir: PathBuf,
    }

    #[cfg(unix)]
    impl DenyLogRemoval {
        fn new(changepacks_dir: &Path, _logs: &[&Path]) -> Self {
            Self::set_mode(changepacks_dir, 0o555);
            Self {
                changepacks_dir: changepacks_dir.to_path_buf(),
            }
        }

        fn set_mode(changepacks_dir: &Path, mode: u32) {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(changepacks_dir, fs::Permissions::from_mode(mode)).unwrap_or_else(
                |err| {
                    panic!(
                        "failed to set mode {mode:o} on {}: {err}",
                        changepacks_dir.display()
                    )
                },
            );
        }
    }

    #[cfg(unix)]
    impl Drop for DenyLogRemoval {
        fn drop(&mut self) {
            Self::set_mode(&self.changepacks_dir, 0o755);
        }
    }

    /// Windows half of [`DenyLogRemoval`]. Windows has no directory-governed
    /// unlink rule, and the read-only ATTRIBUTE is not a lever either: Rust's
    /// `remove_file` passes `FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE`, so a
    /// read-only log is deleted anyway. The sharing rules are the lever — each
    /// log is held open with a share mode of `FILE_SHARE_READ` alone, so the
    /// DELETE-access open inside `remove_file` is refused with
    /// `ERROR_SHARING_VIOLATION`. Dropping the guard closes the handles.
    #[cfg(windows)]
    struct DenyLogRemoval {
        _handles: Vec<std::fs::File>,
    }

    #[cfg(windows)]
    impl DenyLogRemoval {
        fn new(_changepacks_dir: &Path, logs: &[&Path]) -> Self {
            use std::os::windows::fs::OpenOptionsExt;

            /// `FILE_SHARE_READ`: concurrent READ opens are allowed, DELETE
            /// opens are not.
            const FILE_SHARE_READ: u32 = 0x0000_0001;

            Self {
                _handles: logs
                    .iter()
                    .map(|log| {
                        std::fs::OpenOptions::new()
                            .read(true)
                            .share_mode(FILE_SHARE_READ)
                            .open(log)
                            .unwrap_or_else(|err| {
                                panic!("failed to lock {} open: {err}", log.display())
                            })
                    })
                    .collect(),
            }
        }
    }

    /// The multi-failure arm of [`clear_update_logs`]: when MORE THAN ONE log
    /// cannot be deleted, the user must see the count and every failing path in
    /// one aggregated message. This is the final step of `changepacks update`,
    /// and a silently half-cleared `.changepacks/` would re-apply the same
    /// version bumps on the next run — so the count, the `"; "` join across
    /// entries and the whole `Err` construction are pinned here.
    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn clear_update_logs_aggregates_multiple_removal_failures() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // TWO logs, so the plural count and the join across entries both run.
        let first = changepacks_dir.join("changepack_log_a.json");
        let second = changepacks_dir.join("changepack_log_b.json");
        fs::write(&first, r#"{"changes":{},"note":"a"}"#).unwrap();
        fs::write(&second, r#"{"changes":{},"note":"b"}"#).unwrap();

        let logs = [first.as_path(), second.as_path()];
        let guard = DenyLogRemoval::new(&changepacks_dir, &logs);
        let result = clear_update_logs(&changepacks_dir).await;
        // Restore BEFORE asserting so `TempDir` cleanup succeeds on both
        // platforms even if an assertion below panics.
        drop(guard);

        let err = result.expect_err("undeletable update logs must not report success");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("Failed to remove 2 update log(s)"),
            "error must report how many logs survived, got {rendered}"
        );
        assert!(
            rendered.contains("; "),
            "error must join the per-log details with a semicolon, got {rendered}"
        );
        for log in logs {
            assert!(
                rendered.contains(&log.display().to_string()),
                "error must name the surviving log {}, got {rendered}",
                log.display()
            );
            assert!(log.exists(), "{} must have survived", log.display());
        }
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

        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok());
        assert!(!log_file.exists(), "fully applied log should be deleted");
    }

    /// The removal-FAILURE arm of [`clear_applied_update_logs`]: a log whose
    /// `changes` map is fully applied takes the `remove_file` branch, and that
    /// removal can still fail on disk (revoked directory write bit, a held
    /// handle, a locked volume). `changepacks update` must not report success
    /// then, because a log that silently survived deletion re-applies the same
    /// version bumps on the next run. The sibling `clear_update_logs` failure
    /// test pins a DIFFERENT, aggregated message; the per-path
    /// `"Failed to remove update log {path}"` context of the selective cleaner
    /// is pinned only here.
    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn clear_applied_update_logs_reports_removal_failure_with_path() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // EVERY `changes` entry is applied, so the log is classified onto the
        // remove arm rather than the rewrite or the skip arm.
        let log_file = changepacks_dir.join("changepack_log_fully_applied.json");
        let original = br#"{"changes":{"packages/a/package.json":"Patch"},"note":"done"}"#;
        fs::write(&log_file, original).unwrap();

        let logs = [log_file.as_path()];
        let guard = DenyLogRemoval::new(&changepacks_dir, &logs);
        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;
        // Restore BEFORE asserting so `TempDir` cleanup succeeds on both
        // platforms even if an assertion below panics first.
        drop(guard);

        let err = result.expect_err("an undeletable update log must not report success");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("Failed to remove update log"),
            "error chain must carry the removal context message, got {rendered}"
        );
        assert!(
            rendered.contains(&log_file.display().to_string()),
            "error chain must name the offending path {}, got {rendered}",
            log_file.display()
        );
        assert!(
            err.chain()
                .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
            "failure must originate from the removal itself, got {rendered}"
        );
        assert_eq!(
            fs::read(&log_file).unwrap(),
            original,
            "a log that could not be removed must stay byte-identical"
        );
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
        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;
        crate::test_support::set_readonly(&untouched_log, false);

        // Then cleanup succeeds without rewriting the untouched log.
        assert!(result.is_ok(), "selective cleanup failed: {result:?}");
        assert!(!applied_log.exists(), "applied log should be deleted");
        assert_eq!(fs::read(untouched_log).unwrap(), untouched_bytes);
    }

    /// The rewrite-WRITE failure arm: a log whose `changes` map STRADDLES the
    /// applied/unapplied boundary takes the rewrite arm, and the `write` that
    /// executes it can still fail on disk (read-only file, revoked ACL, full
    /// volume). `changepacks update` must not report success in that case,
    /// because a log that silently failed to shrink re-applies the same version
    /// bumps on the next run. Pins that the failure surfaces as an `Err` whose
    /// chain carries the rewrite context, the offending path, and the
    /// underlying `io::Error` — the `io::Error` link is what distinguishes this
    /// arm from the same-worded context on the span-removal step above it.
    #[tokio::test]
    async fn clear_applied_update_logs_reports_rewrite_write_failure_with_path() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // TWO entries with only the FIRST applied, so the log is classified
        // onto the rewrite arm rather than the remove or the skip arm.
        let log_file = changepacks_dir.join("changepack_log_straddling.json");
        let original =
            br#"{"changes":{"a/package.json":"Patch","b/package.json":"Minor"},"note":"keep"}"#;
        fs::write(&log_file, original).unwrap();
        crate::test_support::set_readonly(&log_file, true);

        let applied_paths = HashSet::from([Path::new("a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;
        // Restore BEFORE asserting so `TempDir` cleanup succeeds even if an
        // assertion below panics first.
        crate::test_support::set_readonly(&log_file, false);

        let err = result.expect_err("an unwritable update log must not report success");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("Failed to rewrite update log"),
            "error chain must carry the rewrite context message, got {rendered}"
        );
        assert!(
            rendered.contains(&log_file.display().to_string()),
            "error chain must name the offending path {}, got {rendered}",
            log_file.display()
        );
        assert!(
            err.chain()
                .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()),
            "failure must originate from the rewrite write itself, got {rendered}"
        );
        assert_eq!(
            fs::read(&log_file).unwrap(),
            original,
            "a log that could not be rewritten must stay byte-identical"
        );
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

        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
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

    /// Both execution arms in ONE call. The delete arm and the rewrite arm are
    /// each pinned in isolation above, so nothing yet observes the two buffered
    /// decision lists (`removals`, `rewrites`) being filled from the SAME
    /// classify pass and then drained as two separate `try_join_all` batches.
    /// A future refactor that reserved, drained or ordered those buffers wrongly
    /// — deleting a straddling log, or rewriting a fully applied one — would
    /// still pass every single-arm test. Asserts the exact rewritten BYTES, not
    /// just the parsed shape, so the byte-preservation contract survives the
    /// mixed batch too.
    #[tokio::test]
    async fn clear_applied_update_logs_deletes_and_rewrites_in_one_call() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Log 1: every `changes` entry is applied -> takes the REMOVE arm.
        let full_log = changepacks_dir.join("changepack_log_full.json");
        fs::write(
            &full_log,
            r#"{"changes":{"packages/x/package.json":"Major","packages/y/package.json":"Patch"},"note":"fully applied","date":"2026-01-02"}"#,
        )
        .unwrap();

        // Log 2: straddles the boundary (a applied, b not) -> REWRITE arm.
        // Indented + trailing newline so the byte assertion is meaningful.
        let straddling_log = changepacks_dir.join("changepack_log_straddling.json");
        let straddling_input = "{\n  \"changes\": {\n    \"packages/a/package.json\": \"Patch\",\n    \"packages/b/package.json\": \"Minor\"\n  },\n  \"note\": \"keep this\",\n  \"date\": \"2026-01-01\"\n}\n";
        fs::write(&straddling_log, straddling_input.as_bytes()).unwrap();

        let applied_paths = HashSet::from([
            Path::new("packages/x/package.json"),
            Path::new("packages/y/package.json"),
            Path::new("packages/a/package.json"),
        ]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;
        assert!(result.is_ok(), "mixed cleanup failed: {result:?}");

        // Remove arm ran.
        assert!(
            !full_log.exists(),
            "fully applied log must be deleted in a mixed batch"
        );

        // Rewrite arm ran, and did NOT delete the straddling log.
        assert!(
            straddling_log.exists(),
            "straddling log must be rewritten, not deleted"
        );
        let expected = "{\n  \"changes\": {\n    \"packages/b/package.json\": \"Minor\"\n  },\n  \"note\": \"keep this\",\n  \"date\": \"2026-01-01\"\n}\n";
        assert_eq!(
            fs::read(&straddling_log).unwrap(),
            expected.as_bytes(),
            "indentation, note, date and trailing newline bytes must survive the mixed batch"
        );

        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&straddling_log).unwrap()).unwrap();
        let changes = value["changes"].as_object().unwrap();
        assert_eq!(changes.len(), 1, "only the unapplied entry may remain");
        assert_eq!(changes["packages/b/package.json"], "Minor");
        assert!(changes.get("packages/a/package.json").is_none());
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

        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
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
        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
        let result = clear_applied_update_logs(&changepacks_dir, &applied_paths).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn clear_applied_update_logs_reports_parse_failure_with_path() {
        // A malformed changepack log must fail LOUDLY: the classifier parse is
        // the only thing standing between a corrupt log and a silent no-op, so
        // the error chain has to name both the failing step and the file.
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let log_file = changepacks_dir.join("changepack_log_broken.json");
        fs::write(&log_file, r#"{"changes":{"packages/a/package.json":"#).unwrap();

        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
        let err = clear_applied_update_logs(&changepacks_dir, &applied_paths)
            .await
            .expect_err("a malformed changepack log must not be reported as success");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("Failed to parse update log"),
            "error chain must carry the parse context message, got {rendered}"
        );
        assert!(
            rendered.contains(&log_file.display().to_string()),
            "error chain must name the offending path {}, got {rendered}",
            log_file.display()
        );
        assert!(
            log_file.exists(),
            "a log that failed to parse must not be deleted"
        );
    }

    /// Write `bytes` as a changepack log, run selective cleanup with a
    /// non-empty applied set, and assert the file survived byte-identical.
    async fn assert_selective_clear_leaves_log_untouched(bytes: &[u8]) {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path().join(".changepacks");
        fs::create_dir_all(&changepacks_dir).unwrap();

        let log_file = changepacks_dir.join("changepack_log_1.json");
        fs::write(&log_file, bytes).unwrap();

        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
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
        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
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

        let applied_paths = HashSet::from([Path::new("packages/a/package.json")]);
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
            Path::new("packages/a/package.json"),
            Path::new("packages/c/package.json"),
        ]);
        clear_applied_update_logs(&changepacks_dir, &applied_paths)
            .await
            .unwrap();

        assert_eq!(fs::read(log_file).unwrap(), expected.as_bytes());
    }
}
