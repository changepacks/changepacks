use std::path::{Path, PathBuf};

use anyhow::Result;

pub fn get_relative_path(git_root_path: &Path, absolute_path: &Path) -> Result<PathBuf> {
    match absolute_path.strip_prefix(git_root_path) {
        Ok(relative) => Ok(relative.to_path_buf()),
        Err(_) => Err(anyhow::anyhow!("Failed to get relative path")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_get_relative_path_outside_git_repo() {
        // Create a temporary directory without git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create a test file
        let test_file = temp_path.join("test_file.txt");
        fs::write(&test_file, "test content").unwrap();

        // Test getting relative path (should fail)
        let result = get_relative_path(temp_path, &test_file);
        assert!(result.is_err());
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_relative_path_absolute_path_outside_repo() {
        // Create a temporary directory and initialize git
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Initialize git repository
        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        let inside_path = temp_path.join("inside_absolute_path.txt");
        fs::write(&inside_path, "inside content").unwrap();

        let abs_path = inside_path.canonicalize().unwrap();
        let result = get_relative_path(temp_path, &abs_path);
        assert!(result.is_err());
        // Create another temporary directory outside the git repo
        let outside_dir = TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("outside_file.txt");
        fs::write(&outside_file, "outside content").unwrap();

        // Test getting relative path (should fail)
        let result = get_relative_path(outside_dir.path(), &outside_file);
        assert!(result.is_err());
        temp_dir.close().unwrap();
        outside_dir.close().unwrap();
    }

    #[test]
    fn test_get_relative_path_without_git_repo() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();
        let test_file = temp_path.join("test_file.txt");
        fs::write(&test_file, "test content").unwrap();
        let result = get_relative_path(temp_path, &test_file);
        assert!(result.is_err());
        temp_dir.close().unwrap();
    }
}
