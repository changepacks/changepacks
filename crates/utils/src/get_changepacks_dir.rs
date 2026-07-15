use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::find_current_git_repo;

/// Get the .changepacks directory path from the git repository root
///
/// # Errors
/// Returns error if finding the git repository fails.
pub fn get_changepacks_dir(current_dir: &Path) -> Result<PathBuf> {
    let repo = find_current_git_repo(current_dir)?;
    let changepacks_dir = repo
        .work_dir()
        .context("Git repository has no working directory (bare repository is not supported)")?
        .join(".changepacks");
    Ok(changepacks_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_get_changepacks_dir_success() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        let result = get_changepacks_dir(temp_path);
        assert!(result.is_ok());

        let changepacks_dir = result.unwrap();
        assert!(changepacks_dir.ends_with(".changepacks"));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_changepacks_dir_returns_path_without_creating() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        let result = get_changepacks_dir(temp_path);
        assert!(result.is_ok());

        let changepacks_dir = result.unwrap();

        // Verify the returned path is exactly <repo>/.changepacks
        assert_eq!(changepacks_dir, temp_path.join(".changepacks"));

        // Verify the path does not exist after the call (non-mutating contract)
        assert!(!changepacks_dir.exists());

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_changepacks_dir_without_git_repo() {
        // Create a temporary directory without git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let result = get_changepacks_dir(temp_path);
        assert!(result.is_err());

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_changepacks_dir_path_structure() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        let result = get_changepacks_dir(temp_path);
        assert!(result.is_ok());

        let changepacks_dir = result.unwrap();

        // Verify the path structure
        assert!(changepacks_dir.to_string_lossy().contains(".changepacks"));
        assert!(changepacks_dir.parent().unwrap().exists());

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_changepacks_dir_nested_subdirectory() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        crate::test_support::init_git_repo(temp_path);

        // Create a nested subdirectory
        let nested_dir = temp_path.join("src").join("subdir");
        fs::create_dir_all(&nested_dir).unwrap();

        let result = get_changepacks_dir(&nested_dir);
        assert!(result.is_ok());

        let changepacks_dir = result.unwrap();

        // The changepacks dir should still be at the git root, not in the subdirectory
        assert!(changepacks_dir.to_string_lossy().contains(".changepacks"));
        assert!(changepacks_dir.parent().unwrap() == temp_path);

        temp_dir.close().unwrap();
    }
}
