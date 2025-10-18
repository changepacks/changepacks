use gix::{Repository, discover};

/// Find git repository from current directory using gix
pub fn find_current_git_repo() -> Result<Repository, anyhow::Error> {
    let current_dir = std::env::current_dir()?;
    let repo = discover(&current_dir)?;
    Ok(repo)
}
