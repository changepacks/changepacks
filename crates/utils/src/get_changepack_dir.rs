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
