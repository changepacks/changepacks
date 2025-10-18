use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::find_current_git_repo;

pub fn get_changepack_dir() -> Result<PathBuf> {
    let repo = find_current_git_repo()?;
    let changepack_dir = repo
        .workdir()
        .context("Failed to find current git repository")?
        .join(".changepack");
    Ok(changepack_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_get_changepack_dir_success() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        // Change to the temp directory and test
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_path).unwrap();

        let result = get_changepack_dir();
        assert!(result.is_ok());

        let changepack_dir = result.unwrap();
        assert!(changepack_dir.ends_with(".changepack"));

        // Restore original directory
        std::env::set_current_dir(original_dir).unwrap();
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_changepack_dir_creates_directory() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        // Change to the temp directory and test
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_path).unwrap();

        let result = get_changepack_dir();
        assert!(result.is_ok());

        let changepack_dir = result.unwrap();

        // Create the directory to test that the path is correct
        fs::create_dir_all(&changepack_dir).unwrap();
        assert!(changepack_dir.exists());
        assert!(changepack_dir.is_dir());

        // Restore original directory
        std::env::set_current_dir(original_dir).unwrap();
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_changepack_dir_without_git_repo() {
        // Create a temporary directory without git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Change to the temp directory and test
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_path).unwrap();

        let result = get_changepack_dir();
        assert!(result.is_err());

        // Restore original directory
        std::env::set_current_dir(original_dir).unwrap();
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_changepack_dir_path_structure() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        // Change to the temp directory and test
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_path).unwrap();

        let result = get_changepack_dir();
        assert!(result.is_ok());

        let changepack_dir = result.unwrap();

        // Verify the path structure
        assert!(changepack_dir.to_string_lossy().contains(".changepack"));
        assert!(changepack_dir.parent().unwrap().exists());

        // Restore original directory
        std::env::set_current_dir(original_dir).unwrap();
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_changepack_dir_nested_subdirectory() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        // Create a nested subdirectory
        let nested_dir = temp_path.join("src").join("subdir");
        fs::create_dir_all(&nested_dir).unwrap();

        // Change to the nested directory and test
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&nested_dir).unwrap();

        let result = get_changepack_dir();
        assert!(result.is_ok());

        let changepack_dir = result.unwrap();

        // The changepack dir should still be at the git root, not in the subdirectory
        assert!(changepack_dir.to_string_lossy().contains(".changepack"));
        assert!(changepack_dir.parent().unwrap() == temp_path);

        // Restore original directory
        std::env::set_current_dir(original_dir).unwrap();
        temp_dir.close().unwrap();
    }
}
