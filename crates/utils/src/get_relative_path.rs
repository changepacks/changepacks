use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Get the relative path from a git root to an absolute path, returning a borrowed reference.
///
/// This is the zero-copy variant; use when the result is only needed for lookups
/// (e.g., `HashMap<PathBuf, V>::get(&path)`). For owned ownership, use [`get_relative_path`].
///
/// # Errors
/// Returns error if the absolute path is not within the git root directory.
pub fn get_relative_path_ref<'a>(
    git_root_path: &Path,
    absolute_path: &'a Path,
) -> Result<&'a Path> {
    // The context message is a pinned contract: `test_get_relative_path_ref_error_context_names_both_paths`
    // below asserts it byte-for-byte, so the wording must not drift.
    absolute_path.strip_prefix(git_root_path).with_context(|| {
        format!(
            "Failed to get relative path: '{}' is not within '{}'",
            absolute_path.display(),
            git_root_path.display()
        )
    })
}

/// Get the relative path from a git root to an absolute path, returning an owned `PathBuf`.
///
/// For lookup-only use cases, prefer [`get_relative_path_ref`] to avoid allocation.
///
/// # Errors
/// Returns error if the absolute path is not within the git root directory.
pub fn get_relative_path(git_root_path: &Path, absolute_path: &Path) -> Result<PathBuf> {
    get_relative_path_ref(git_root_path, absolute_path).map(Path::to_path_buf)
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

        // Create a test file path
        let outside_dir = TempDir::new().unwrap();
        let test_file = outside_dir.path().join("test_file.txt");

        // Test getting relative path (should fail)
        let result = get_relative_path(temp_path, &test_file);
        assert!(result.is_err());
        temp_dir.close().unwrap();
    }

    #[test]
    fn test_get_relative_path_absolute_path_outside_repo() {
        // Create a temporary directory.
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let inside_path = temp_path.join("inside_absolute_path.txt");
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

    /// The zero-copy variant is what the CLI calls directly, so exercise it
    /// without going through the owned wrapper.
    #[test]
    fn test_get_relative_path_ref_returns_borrowed_suffix() {
        let root = PathBuf::from("repo");
        let absolute = root.join("crates").join("utils").join("Cargo.toml");
        let result = get_relative_path_ref(&root, &absolute).unwrap();
        assert_eq!(
            result,
            PathBuf::from("crates").join("utils").join("Cargo.toml")
        );
    }

    /// The doc comment declares the failure text as a stability contract, so
    /// pin the literal prefix together with both interpolated paths.
    #[test]
    fn test_get_relative_path_ref_error_context_names_both_paths() {
        let git_root_path = PathBuf::from("repo");
        let absolute_path = PathBuf::from("other").join("package.json");

        let err = get_relative_path_ref(&git_root_path, &absolute_path)
            .expect_err("a path outside the git root must fail");

        let chain = format!("{err:#}");
        assert!(
            chain.contains(&format!(
                "Failed to get relative path: '{}' is not within '{}'",
                absolute_path.display(),
                git_root_path.display()
            )),
            "error chain should carry the documented message and both paths, got: {chain}"
        );
    }
}
