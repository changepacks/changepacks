use std::path::Path;

use anyhow::Result;
use gix::{ThreadSafeRepository, discover};

/// Find git repository from current directory using gix
pub fn find_current_git_repo(current_dir: &Path) -> Result<ThreadSafeRepository> {
    let repo = discover(current_dir)?.into_sync();
    Ok(repo)
}
