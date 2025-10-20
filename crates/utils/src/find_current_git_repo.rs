use std::path::Path;

use gix::{Repository, discover};

/// Find git repository from current directory using gix
pub fn find_current_git_repo(current_dir: &Path) -> Result<Repository, anyhow::Error> {
    let repo = discover(current_dir)?;
    Ok(repo)
}
