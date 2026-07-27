use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use changepacks_core::has_extension_ignore_ascii_case;
use tokio::fs::{read_dir, read_to_string};

/// Single anyhow context message for every "Failed to read changepacks directory"
/// error path in `collect_changepack_log_paths` — both the `read_dir` failure
/// and the `next_entry` failure. Routing both sites through one helper ensures
/// the message can never drift between them.
fn read_dir_context(dir: &Path) -> String {
    format!("Failed to read changepacks directory {}", dir.display())
}

/// Returns `true` iff `file_name` matches `changepack_log_*.json`, with a
/// case-insensitive extension match.
///
/// Consumed transitively by [`crate::clear_update_logs`] and
/// [`crate::clear_applied_update_logs`] (the cleaners) and [`crate::gen_update_map`]
/// (the reader) through [`collect_changepack_log_paths`], ensuring a future change
/// to what counts as a changepack log updates all three in lock-step. Any drift
/// between the filters would either (a) let the cleaner wipe a file the reader was
/// still going to parse, or (b) let the reader parse a file the cleaner would have
/// deleted — both silent-data-loss shapes.
#[must_use]
fn is_changepack_log_json_name(file_name: &str) -> bool {
    file_name.starts_with("changepack_log_")
        && has_extension_ignore_ascii_case(Path::new(file_name), "json")
}

/// Collect all changepack log file paths from the `.changepacks/` directory.
///
/// Walks the directory once, filtering by [`is_changepack_log_json_name`],
/// and returns the collected paths. Used by [`crate::gen_update_map`],
/// [`crate::clear_update_logs`], and [`crate::clear_applied_update_logs`]
/// to ensure all three sites use the same predicate.
///
/// # Errors
/// Returns an error if the directory exists but cannot be read. A missing
/// directory is not an error — `read_dir` reports `NotFound`, which is mapped
/// to an empty list so callers need not pre-check the directory's existence.
pub async fn collect_changepack_log_paths(changepacks_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = match read_dir(changepacks_dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| read_dir_context(changepacks_dir));
        }
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    while let Some(file) = entries
        .next_entry()
        .await
        .with_context(|| read_dir_context(changepacks_dir))?
    {
        let file_name = file.file_name();
        if is_changepack_log_json_name(file_name.to_string_lossy().as_ref()) {
            let path = file.path();
            let file_type = file.file_type().await.with_context(|| {
                format!(
                    "Failed to read metadata for changepack log candidate {} in directory {}",
                    path.display(),
                    changepacks_dir.display()
                )
            })?;
            if file_type.is_file() {
                paths.push(path);
            }
        }
    }
    // Sort paths to ensure deterministic order: `read_dir` order is filesystem-dependent,
    // so sorting makes downstream `logs` output order stable across runs and platforms.
    paths.sort_unstable();
    Ok(paths)
}

/// Concurrently read the bodies of every collected changepack-log path.
///
/// Shared by [`crate::gen_update_map`] (the reader) and
/// [`crate::clear_applied_update_logs`] (the selective cleaner): both need the
/// full set of log bodies read in one `try_join_all` batch and differ ONLY in
/// the human-facing `label` woven into the error context (`"changepack log"`
/// vs `"update log"`). Threading `label` keeps each caller's error message
/// byte-identical while collapsing the previously copy-pasted read loop into a
/// single well-named helper.
///
/// # Errors
/// Returns the first read failure, contextualized with `label` and the
/// offending path — exactly as the two inlined loops did before.
pub(crate) async fn read_log_bodies(paths: &[PathBuf], label: &str) -> Result<Vec<String>> {
    futures::future::try_join_all(paths.iter().map(|path| async move {
        read_to_string(path)
            .await
            .with_context(|| format!("Failed to read {label} {}", path.display()))
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use tempfile::TempDir;
    use tokio::fs;

    // Every `.changepacks/` filename shape the cleaner + reader must agree on.
    #[rstest]
    // `config.json` is the one JSON file inside `.changepacks/` that must
    // NOT be treated as a changepack log by either the cleaner or the
    // reader; it stores the user's config. Windows (NTFS) and macOS
    // (default HFS+/APFS) return the same on-disk `config.json` from
    // `read_dir` under mixed-case names — the guard MUST reject every
    // case variant so the cleaner can never silently delete the user's
    // config file.
    #[case("config.json", false)]
    #[case("Config.json", false)]
    #[case("CONFIG.JSON", false)]
    #[case("config.JSON", false)]
    #[case("CoNfIg.JsOn", false)]
    // Non-JSON files (`.gitkeep`, `README.md`, dotfiles) are user-owned
    // and must survive `clear_update_logs`; the reader skips them for
    // the same reason. `randomfile` has no extension at all.
    #[case("README.md", false)]
    #[case(".gitkeep", false)]
    #[case(".gitignore", false)]
    #[case("notes.txt", false)]
    #[case("randomfile", false)]
    // Only generated changepack-log names are logs. Arbitrary JSON files are
    // user-owned and must not be read or deleted.
    #[case("update_log_1.json", false)]
    #[case("changepack_log_2.json", true)]
    #[case("2024-01-01.json", false)]
    #[case("changepack_log_2.JSON", true)]
    #[case("update.Json", false)]
    #[case("notes.json", false)]
    #[case("changepack_log.json", false)]
    fn test_is_changepack_log_json_name(#[case] file_name: &str, #[case] expected: bool) {
        assert_eq!(is_changepack_log_json_name(file_name), expected);
    }

    // Regression: `collect_changepack_log_paths` must return paths in lexicographic
    // order regardless of filesystem read_dir order. This test writes files in
    // reverse lexicographic order (b before a) to verify sorting is applied.
    #[tokio::test]
    async fn test_collect_changepack_log_paths_deterministic_order() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path();

        // Write changepack_log_b.json FIRST, then changepack_log_a.json
        // (creation order differs from lexicographic order).
        fs::write(
            changepacks_dir.join("changepack_log_b.json"),
            r#"{"changes": {}, "note": "note_b"}"#,
        )
        .await
        .unwrap();
        fs::write(
            changepacks_dir.join("changepack_log_a.json"),
            r#"{"changes": {}, "note": "note_a"}"#,
        )
        .await
        .unwrap();
        fs::write(
            changepacks_dir.join("config.json"),
            r#"{"baseBranch": "main"}"#,
        )
        .await
        .unwrap();
        fs::write(changepacks_dir.join("README.md"), "not a changepack log")
            .await
            .unwrap();
        fs::create_dir(changepacks_dir.join("not_a_log.json"))
            .await
            .unwrap();

        let paths = collect_changepack_log_paths(changepacks_dir).await.unwrap();

        // Paths must be sorted lexicographically (a before b), not in creation order.
        assert_eq!(paths.len(), 2);
        assert!(
            paths[0].ends_with("changepack_log_a.json"),
            "expected first path to be changepack_log_a.json, got {:?}",
            paths[0]
        );
        assert!(
            paths[1].ends_with("changepack_log_b.json"),
            "expected second path to be changepack_log_b.json, got {:?}",
            paths[1]
        );
    }

    // Covers the non-`NotFound` `read_dir` error arm: a MISSING directory is
    // deliberately mapped to an empty list, but any OTHER `read_dir` failure must
    // surface as an error carrying `read_dir_context`. Pointing `read_dir` at a
    // regular file produces a deterministic non-`NotFound` io error on both Windows
    // (`ERROR_DIRECTORY`) and Unix (`ENOTDIR`), so this exercises the arm without
    // relying on permission tricks that differ per platform.
    //
    // Without this test the arm could silently degrade to `Ok(Vec::new())` — which
    // would make `update` see zero changepack logs on an unreadable `.changepacks/`
    // and report "nothing to update" instead of failing loudly.
    #[tokio::test]
    async fn test_collect_changepack_log_paths_read_dir_error_is_contextualized() {
        let temp_dir = TempDir::new().unwrap();
        let not_a_dir = temp_dir.path().join("not-a-dir");
        fs::write(&not_a_dir, "regular file, not a directory")
            .await
            .unwrap();

        let err = collect_changepack_log_paths(&not_a_dir)
            .await
            .expect_err("read_dir on a regular file must not be reported as success");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("Failed to read changepacks directory"),
            "error chain must carry the read_dir context message, got {rendered}"
        );
        assert!(
            rendered.contains(&not_a_dir.display().to_string()),
            "error chain must name the offending path {}, got {rendered}",
            not_a_dir.display()
        );
    }
}
