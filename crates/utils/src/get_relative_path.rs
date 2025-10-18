use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::find_current_git_repo;

pub fn get_relative_path(absolute_path: &str) -> Result<String> {
    let git_repo = find_current_git_repo()
        .context("Git repository not found")?
        .workdir()
        .context("Git repository workdir not found")?
        .to_path_buf();
    let git_root = git_repo.to_path_buf();
    let project_path = PathBuf::from(absolute_path);
    match project_path.strip_prefix(&git_root) {
        Ok(relative) => Ok(format!("./{}", relative.to_string_lossy())),
        Err(_) => Err(anyhow::anyhow!("Failed to get relative path")),
    }
}
