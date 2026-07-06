use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::fs::{read_dir, remove_file};

use crate::is_changepack_log_json_name;

/// Remove all update logs without confirmation
///
/// Uses [`is_changepack_log_json_name`] — the same predicate
/// [`gen_update_map`](crate::gen_update_map) uses — so the cleaner and the
/// reader stay in lock-step. A file the reader intentionally ignores
/// (`.gitkeep`, `README.md`, etc.) is not silently destroyed the first time
/// `update` completes.
///
/// # Errors
/// Returns error if any update log file fails to be removed.
pub async fn clear_update_logs(changepacks_dir: &Path) -> Result<()> {
    match tokio::fs::try_exists(changepacks_dir).await {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to check changepacks directory {}",
                    changepacks_dir.display()
                )
            });
        }
    }
    // Two-phase collect+delete, mirroring `gen_update_map`:
    //   Phase 1: single directory walk to collect the paths of every matching
    //            `changepack_log_*.json` entry — pure name filtering, no IO body.
    //   Phase 2: build a `Vec::with_capacity(paths.len())` of `remove_file`
    //            futures and hand them to `futures::future::join_all` as today.
    //   Keeping the reader (`gen_update_map`) and cleaner (`clear_update_logs`)
    //   in lock-step on the shape reinforces the shared-predicate invariant
    //   and drops the geometric-doubling reallocations the un-hinted
    //   `vec![]` incurs on repos with many pending changepack logs.
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut entries = read_dir(changepacks_dir).await?;
    while let Some(file) = entries.next_entry().await? {
        let file_name = file.file_name();
        let file_name_lossy = file_name.to_string_lossy();
        if !is_changepack_log_json_name(file_name_lossy.as_ref()) {
            continue;
        }
        paths.push(file.path());
    }
    let mut error_details = Vec::new();
    for result in futures::future::join_all(paths.into_iter().map(remove_file)).await {
        if let Err(err) = result {
            error_details.push(err.to_string());
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
        let log_file1 = changepacks_dir.join("update_log_1.json");
        let log_file2 = changepacks_dir.join("update_log_2.json");
        let log_file3 = changepacks_dir.join("update_log_3.json");
        fs::write(&log_file1, r#"{"changes": {}, "note": "test1"}"#).unwrap();
        fs::write(&log_file2, r#"{"changes": {}, "note": "test2"}"#).unwrap();
        fs::write(&log_file3, r#"{"changes": {}, "note": "test3"}"#).unwrap();

        // Test clearing logs
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());

        // config.json should remain
        assert!(config_file.exists(), "config.json should not be deleted");

        // All update log files should be deleted
        assert!(!log_file1.exists(), "update_log_1.json should be deleted");
        assert!(!log_file2.exists(), "update_log_2.json should be deleted");
        assert!(!log_file3.exists(), "update_log_3.json should be deleted");
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

        // Create various update log files with different names
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

        // All update log files should be deleted
        assert!(!log_file1.exists(), "2024-01-01.json should be deleted");
        assert!(!log_file2.exists(), "2024-01-02.json should be deleted");
        assert!(!log_file3.exists(), "update.json should be deleted");
        assert!(!log_file4.exists(), "log.json should be deleted");
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
        let log_file1 = changepacks_dir.join("update_log_1.json");
        let log_file2 = changepacks_dir.join("update_log_2.json");
        fs::write(&log_file1, r#"{"changes": {}, "note": "test1"}"#).unwrap();
        fs::write(&log_file2, r#"{"changes": {}, "note": "test2"}"#).unwrap();

        // Test clearing logs
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_ok());

        // All update log files should be deleted
        assert!(!log_file1.exists(), "update_log_1.json should be deleted");
        assert!(!log_file2.exists(), "update_log_2.json should be deleted");
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
        let log_file = changepacks_dir.join("update_log_1.json");
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
        assert!(!log_file.exists(), "update_log_1.json must be deleted");
    }

    #[tokio::test]
    async fn test_clear_update_logs_file_deletion_failure() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create .changepacks directory
        let changepacks_dir = get_changepacks_dir(temp_path).unwrap();
        fs::create_dir_all(&changepacks_dir).unwrap();

        // Create a subdirectory with a name that looks like a JSON file
        // This will cause remove_file to fail because it's a directory, not a file
        let log_dir = changepacks_dir.join("update_log.json");
        fs::create_dir_all(&log_dir).unwrap();

        // Test clearing logs - should fail because we're trying to remove a directory
        let result = clear_update_logs(&changepacks_dir).await;
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Failed to remove 1 update log(s)"));
    }
}
