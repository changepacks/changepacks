use std::path::Path;

use anyhow::{Context, Result};
use gix::{ThreadSafeRepository, discover};

/// Find git repository from current directory using gix
///
/// # Errors
/// Returns error if the current directory is not in a git repository. The
/// error is wrapped with the `current_dir` we started discovery from so CLI
/// users see WHICH directory failed to resolve to a git repository, matching
/// the `.context("Not a git working directory ...")` pattern already used at
/// the `CommandContext::new` boundary.
pub fn find_current_git_repo(current_dir: &Path) -> Result<ThreadSafeRepository> {
    let repo = discover(current_dir)
        .with_context(|| {
            format!(
                "Failed to discover git repository from {}",
                current_dir.display()
            )
        })?
        .into_sync();
    Ok(repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    #[test]
    fn test_find_current_git_repo_without_git_repo() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        let result = find_current_git_repo(temp_path);
        assert!(result.is_err());
        // Lock the improved error context: the outer message must name the
        // starting directory so CLI users know WHICH path failed to resolve.
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Failed to discover git repository from"),
            "expected context prefix in error, got: {msg}"
        );
        assert!(
            msg.contains(&temp_path.display().to_string()),
            "expected temp path in error, got: {msg}"
        );
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_current_git_repo_with_git_repo() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        {
            let result = find_current_git_repo(temp_path);
            assert!(result.is_ok());
            let repo = result.unwrap();
            assert!(repo.work_dir().unwrap() == temp_path);
        }
        {
            fs::create_dir_all(&temp_path.join("subdir")).await.unwrap();
            let result = find_current_git_repo(&temp_path.join("subdir"));
            println!("{:?}", result);
            assert!(result.is_ok());
            let repo = result.unwrap();
            assert!(repo.work_dir().unwrap() == temp_path);
        }
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_find_current_git_repo_from_deeply_nested_subdir() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        let deep_subdir = temp_path.join("a").join("b").join("c").join("d");
        fs::create_dir_all(&deep_subdir).await.unwrap();

        let result = find_current_git_repo(&deep_subdir);
        assert!(result.is_ok());
        let repo = result.unwrap();
        assert!(repo.work_dir().unwrap() == temp_path);

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_find_current_git_repo_root_has_git_dir() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::process::Command::new("git")
            .arg("init")
            .current_dir(temp_path)
            .output()
            .unwrap();

        let repo = find_current_git_repo(temp_path).unwrap();
        let work_dir = repo.work_dir().unwrap();
        // The .git directory must exist at the discovered repo root
        assert!(work_dir.join(".git").exists());

        temp_dir.close().unwrap();
    }
}
