use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::fs::read_dir;

/// Returns `true` iff `file_name` names a changepack log JSON file — i.e. it
/// is NOT `config.json` and its extension matches `.json` case-insensitively.
///
/// Both comparisons are ASCII case-insensitive because Windows (NTFS) and
/// macOS (default HFS+/APFS) surface the same on-disk config file to
/// `read_dir` under mixed-case names like `Config.json` or `CONFIG.JSON`. A
/// case-sensitive `file_name != "config.json"` guard would mistakenly treat
/// those variants as changepack logs and let `clear_update_logs` silently
/// delete the user's config file — a data-loss shape.
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
    !file_name.eq_ignore_ascii_case("config.json")
        && Path::new(file_name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
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
pub(crate) async fn collect_changepack_log_paths(changepacks_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = match read_dir(changepacks_dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to read changepacks directory {}",
                    changepacks_dir.display()
                )
            });
        }
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    while let Some(file) = entries.next_entry().await? {
        let file_name = file.file_name();
        if is_changepack_log_json_name(file_name.to_string_lossy().as_ref()) {
            paths.push(file.path());
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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
    // Real changepack log names — both the auto-numbered and any custom
    // JSON file — must be recognized as logs. Last two exercise the
    // case-insensitive extension match (matches the historical filter).
    #[case("update_log_1.json", true)]
    #[case("changepack_log_2.json", true)]
    #[case("2024-01-01.json", true)]
    #[case("changepack_log_2.JSON", true)]
    #[case("update.Json", true)]
    fn test_is_changepack_log_json_name(#[case] file_name: &str, #[case] expected: bool) {
        assert_eq!(is_changepack_log_json_name(file_name), expected);
    }
}
