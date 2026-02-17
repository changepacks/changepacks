use std::path::Path;

use anyhow::Result;
use gix::{ThreadSafeRepository, discover};

/// Find git repository from current directory using gix
///
/// # Errors
/// Returns error if the current directory is not in a git repository.
pub fn find_current_git_repo(current_dir: &Path) -> Result<ThreadSafeRepository> {
    let repo = discover(current_dir)?.into_sync();
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
