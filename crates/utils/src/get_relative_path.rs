use std::path::{Path, PathBuf};

use anyhow::Result;

/// Get the relative path from a git root to an absolute path
///
/// # Errors
/// Returns error if the absolute path is not within the git root directory.
pub fn get_relative_path(git_root_path: &Path, absolute_path: &Path) -> Result<PathBuf> {
    match absolute_path.strip_prefix(git_root_path) {
        Ok(relative) => Ok(relative.to_path_buf()),
        Err(_) => Err(anyhow::anyhow!(
            "Failed to get relative path: '{}' is not within '{}'",
            absolute_path.display(),
            git_root_path.display()
        )),
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
        let outside_dir = TempDir::new().unwrap();
        let test_file = outside_dir.path().join("test_file.txt");
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

        let abs_path = inside_path;
        let result = get_relative_path(temp_path, &abs_path);
        assert!(result.is_ok());
        // Create another temporary directory outside the git repo
        let outside_dir = TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("outside_file.txt");
        fs::write(&outside_file, "outside content").unwrap();
        let outside_file = outside_file.canonicalize().unwrap();

        // Test getting relative path (should fail)
        let result = get_relative_path(temp_path, &outside_file);
        assert!(result.is_err());
        temp_dir.close().unwrap();
        outside_dir.close().unwrap();
    }

    #[test]
    fn test_get_relative_path_valid_nested_path() {
        let root = PathBuf::from("repo");
        let absolute = root.join("packages").join("foo").join("package.json");
        let result = get_relative_path(&root, &absolute).unwrap();
        assert_eq!(
            result,
            PathBuf::from("packages").join("foo").join("package.json")
        );
    }

    #[test]
    fn test_get_relative_path_at_root_level() {
        let root = PathBuf::from("repo");
        let absolute = root.join("package.json");
        let result = get_relative_path(&root, &absolute).unwrap();
        assert_eq!(result, PathBuf::from("package.json"));
    }

    #[test]
    fn test_get_relative_path_deeply_nested() {
        let root = PathBuf::from("repo");
        let absolute = root
            .join("a")
            .join("b")
            .join("c")
            .join("d")
            .join("e")
            .join("package.json");
        let result = get_relative_path(&root, &absolute).unwrap();
        assert_eq!(
            result,
            PathBuf::from("a")
                .join("b")
                .join("c")
                .join("d")
                .join("e")
                .join("package.json")
        );
    }

    #[test]
    fn test_get_relative_path_same_path() {
        let root = PathBuf::from("repo");
        let result = get_relative_path(&root, &root).unwrap();
        assert_eq!(result, PathBuf::from(""));
    }

    #[test]
    fn test_get_relative_path_with_real_tempdir() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let absolute = root.join("src").join("lib.rs");
        let result = get_relative_path(root, &absolute).unwrap();
        assert_eq!(result, PathBuf::from("src").join("lib.rs"));
    }
}
