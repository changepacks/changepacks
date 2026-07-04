use std::path::Path;

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
/// Shared by [`crate::clear_update_logs`] (the cleaner) and
/// [`crate::gen_update_map`] (the reader) so a future change to what counts
/// as a changepack log updates both in lock-step. Any drift between the two
/// filters would either (a) let the cleaner wipe a file the reader was still
/// going to parse, or (b) let the reader parse a file the cleaner would have
/// deleted — both silent-data-loss shapes.
#[must_use]
pub(crate) fn is_changepack_log_json_name(file_name: &str) -> bool {
    !file_name.eq_ignore_ascii_case("config.json")
        && Path::new(file_name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rejects_config_json() {
        // `config.json` is the one JSON file inside `.changepacks/` that must
        // NOT be treated as a changepack log by either the cleaner or the
        // reader; it stores the user's config.
        assert!(!is_changepack_log_json_name("config.json"));
    }

    #[test]
    fn test_rejects_config_json_case_insensitive() {
        // Windows (NTFS) and macOS (default HFS+/APFS) return the same
        // on-disk `config.json` from `read_dir` under mixed-case names —
        // the guard MUST reject every case variant so the cleaner can never
        // silently delete the user's config file.
        assert!(!is_changepack_log_json_name("Config.json"));
        assert!(!is_changepack_log_json_name("CONFIG.JSON"));
        assert!(!is_changepack_log_json_name("config.JSON"));
        assert!(!is_changepack_log_json_name("CoNfIg.JsOn"));
    }

    #[test]
    fn test_rejects_non_json_files() {
        // Non-JSON files (`.gitkeep`, `README.md`, dotfiles) are user-owned
        // and must survive `clear_update_logs`; the reader skips them for
        // the same reason.
        assert!(!is_changepack_log_json_name("README.md"));
        assert!(!is_changepack_log_json_name(".gitkeep"));
        assert!(!is_changepack_log_json_name(".gitignore"));
        assert!(!is_changepack_log_json_name("notes.txt"));
        // No extension at all.
        assert!(!is_changepack_log_json_name("randomfile"));
    }

    #[test]
    fn test_accepts_json_logs() {
        // Real changepack log names — both the auto-numbered and any custom
        // JSON file — must be recognized as logs.
        assert!(is_changepack_log_json_name("update_log_1.json"));
        assert!(is_changepack_log_json_name("changepack_log_2.json"));
        assert!(is_changepack_log_json_name("2024-01-01.json"));
        // Case-insensitive extension match (matches the historical filter).
        assert!(is_changepack_log_json_name("changepack_log_2.JSON"));
        assert!(is_changepack_log_json_name("update.Json"));
    }
}
