use gix::Repository;
use gix::discover;
use std::path::PathBuf;

/// Find git repository from current directory using gix
pub fn find_current_git_repo() -> Result<Repository, anyhow::Error> {
    let current_dir = std::env::current_dir()?;
    let path = current_dir;
    let repo = discover(&path)?;
    Ok(repo)
}

/// Find git repository from specific path using gix
pub fn find_git_repo(start_path: &str) -> Result<Repository, anyhow::Error> {
    let path = PathBuf::from(start_path);
    let repo = discover(&path)?;
    Ok(repo)
}
