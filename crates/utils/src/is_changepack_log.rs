use std::ffi::OsStr;
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

/// Returns `true` iff the raw `file_name` starts with the pure-ASCII `prefix`.
///
/// THE single filename-prefix policy for every `.changepacks/` filename gate:
/// the `changepack_log_` gate in [`is_changepack_log_json_name`] below and the
/// [`crate::CARRY_FORWARD_LOG_PREFIX`] gate in [`crate::gen_update_map`]. Both
/// classify entries of the same directory, so a drift between two hand-rolled
/// copies would silently split one policy into two.
///
/// Takes an [`OsStr`] and gates on [`OsStr::as_encoded_bytes`] so the per-entry
/// `to_string_lossy` scan-and-maybe-allocate is gone: the sought prefixes are
/// pure ASCII, Unix bytes and Windows WTF-8 encode ASCII identically, and
/// `to_string_lossy` only ever rewrites *invalid* sequences (into non-ASCII
/// `U+FFFD`), so it can neither create nor destroy an ASCII-prefix match and the
/// encoded-bytes gate accepts exactly the same names.
///
/// Callers must pass an ASCII-only `prefix`; a non-ASCII one would compare raw
/// UTF-8 bytes against a platform-specific encoding and is not a supported input.
#[must_use]
pub(crate) fn file_name_has_ascii_prefix(file_name: &OsStr, prefix: &str) -> bool {
    debug_assert!(
        prefix.is_ascii(),
        "file_name_has_ascii_prefix requires an ASCII prefix, got {prefix:?}"
    );
    file_name.as_encoded_bytes().starts_with(prefix.as_bytes())
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
///
/// The prefix half is delegated to [`file_name_has_ascii_prefix`], which owns the
/// encoded-bytes policy shared with the carry-forward gate.
#[must_use]
fn is_changepack_log_json_name(file_name: &OsStr) -> bool {
    file_name_has_ascii_prefix(file_name, "changepack_log_")
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
        if is_changepack_log_json_name(&file_name) {
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
        assert_eq!(
            is_changepack_log_json_name(OsStr::new(file_name)),
            expected,
            "is_changepack_log_json_name({file_name:?})"
        );
    }

    /// Splice one INVALID encoding unit between two pure-ASCII halves to build a
    /// file name that no lossless `&str` view can represent: a lone `0xFF` byte
    /// on Unix, an unpaired high surrogate on Windows (WTF-8).
    #[cfg(unix)]
    fn non_unicode_name(prefix: &str, suffix: &str) -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;

        let mut bytes = prefix.as_bytes().to_vec();
        bytes.push(0xFF);
        bytes.extend_from_slice(suffix.as_bytes());
        std::ffi::OsString::from_vec(bytes)
    }

    /// Windows counterpart of the Unix `non_unicode_name` above.
    #[cfg(windows)]
    fn non_unicode_name(prefix: &str, suffix: &str) -> std::ffi::OsString {
        use std::os::windows::ffi::OsStringExt;

        let mut units: Vec<u16> = prefix.encode_utf16().collect();
        units.push(0xD800);
        units.extend(suffix.encode_utf16());
        std::ffi::OsString::from_wide(&units)
    }

    // Pins the equivalence that lets the prefix gate read
    // `OsStr::as_encoded_bytes` instead of `to_string_lossy`: names that are NOT
    // valid Unicode are the ONLY inputs where the two views can differ, since
    // that is exactly when `to_string_lossy` rewrites bytes (and allocates).
    // Both halves of each name are pure ASCII, which Unix bytes and Windows
    // WTF-8 encode identically, so the verdict must be unchanged: an invalid
    // unit inside the stem is accepted (prefix and `.json` extension intact) and
    // an invalid unit inside the extension is rejected (lossy replacement yields
    // non-ASCII `U+FFFD`, never `json`). `read_dir` can hand these names to
    // `collect_changepack_log_paths` on any filesystem that does not validate
    // Unicode.
    #[cfg(any(unix, windows))]
    #[test]
    fn test_is_changepack_log_json_name_non_unicode_names() {
        let in_stem = non_unicode_name("changepack_log_a", ".json");
        assert!(
            in_stem.to_str().is_none(),
            "test precondition: {in_stem:?} must not be valid Unicode"
        );
        assert!(
            is_changepack_log_json_name(&in_stem),
            "an invalid unit in the stem must not hide the ASCII prefix or extension: {in_stem:?}"
        );

        let in_extension = non_unicode_name("changepack_log_a.js", "on");
        assert!(
            in_extension.to_str().is_none(),
            "test precondition: {in_extension:?} must not be valid Unicode"
        );
        assert!(
            !is_changepack_log_json_name(&in_extension),
            "an invalid unit inside the extension must never read as \"json\": {in_extension:?}"
        );
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

    // Directly pins the `file_type.is_file()` gate inside
    // `collect_changepack_log_paths`: a DIRECTORY whose name matches the
    // changepack-log pattern (`changepack_log_dir.json`) must never be
    // collected. `is_changepack_log_json_name` is a pure name predicate and
    // says `true` for that name, so only the entry-type gate rejects it.
    // Without this test the gate is exercised only transitively through
    // `clear_update_logs`; the `gen_update_map` reader and the `update`
    // rollback snapshot consume the same collector and would try to
    // `read_to_string` a directory (`EISDIR`/`ERROR_ACCESS_DENIED`), turning a
    // stray directory into a hard failure of `check`/`update`.
    //
    // The same test pins the case-insensitive extension contract at the
    // COLLECTOR level: `changepack_log_b.JSON` is a real file and MUST be
    // collected, so a future "just compare the literal .json suffix" rewrite
    // of the collector cannot pass while only the name-predicate table above
    // stays green.
    #[tokio::test]
    async fn test_collect_changepack_log_paths_skips_directory_named_like_a_log() {
        let temp_dir = TempDir::new().unwrap();
        let changepacks_dir = temp_dir.path();

        fs::write(
            changepacks_dir.join("changepack_log_a.json"),
            r#"{"changes": {}, "note": "note_a"}"#,
        )
        .await
        .unwrap();
        fs::write(
            changepacks_dir.join("changepack_log_b.JSON"),
            r#"{"changes": {}, "note": "note_b"}"#,
        )
        .await
        .unwrap();
        // A directory whose name passes `is_changepack_log_json_name`.
        fs::create_dir(changepacks_dir.join("changepack_log_dir.json"))
            .await
            .unwrap();

        assert!(
            is_changepack_log_json_name(OsStr::new("changepack_log_dir.json")),
            "test precondition: the name predicate must accept the directory name, \
             otherwise this test would not reach the file_type gate"
        );

        let paths = collect_changepack_log_paths(changepacks_dir).await.unwrap();

        let names: Vec<String> = paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "changepack_log_a.json".to_string(),
                "changepack_log_b.JSON".to_string(),
            ],
            "only regular files may be collected: the directory \
             changepack_log_dir.json must be skipped and the uppercase-extension \
             file must be kept, got {paths:?}"
        );
    }

    // Covers the `ErrorKind::NotFound` `read_dir` arm, which the doc comment on
    // `collect_changepack_log_paths` declares as a contract: a missing directory is
    // NOT an error, it is an empty list. Production callers rely on that contract
    // and never pre-check the directory's existence — `gen_update_map`,
    // `clear_update_logs`, `clear_applied_update_logs`, and the `update` rollback
    // snapshot all propagate with `?`, so a regression turning this arm into an
    // `Err` would break `check`/`update` in every repository that has not created
    // `.changepacks/` yet. The joined directory is deliberately never created.
    #[tokio::test]
    async fn test_collect_changepack_log_paths_missing_directory_is_empty() {
        let temp_dir = TempDir::new().unwrap();
        let missing_dir = temp_dir.path().join("never-created");
        assert!(
            !missing_dir.exists(),
            "test precondition: {} must not exist so read_dir reports NotFound",
            missing_dir.display()
        );

        let paths = collect_changepack_log_paths(&missing_dir)
            .await
            .expect("a missing changepacks directory must map to Ok, not Err");

        assert!(
            paths.is_empty(),
            "a missing changepacks directory must yield no log paths, got {paths:?}"
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
