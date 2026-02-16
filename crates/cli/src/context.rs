use crate::finders::get_finders;
use anyhow::{Context, Result};
use changepacks_core::Config;
use changepacks_core::ProjectFinder;
use changepacks_utils::{find_current_git_repo, find_project_dirs, get_changepacks_config};
use std::path::PathBuf;

pub struct CommandContext {
    pub repo_root_path: PathBuf,
    pub config: Config,
    pub project_finders: Vec<Box<dyn ProjectFinder>>,
}

impl CommandContext {
    /// # Errors
    /// Returns error if finding git repository or discovering projects fails.
    pub async fn new(remote: bool) -> Result<Self> {
        let current_dir = std::env::current_dir()?;
        let repo = find_current_git_repo(&current_dir)?;
        let repo_root_path = repo
            .work_dir()
            .context("Not a git working directory. Ensure you are inside a git repository.")?
            .to_path_buf();
        let config = get_changepacks_config(&current_dir).await?;
        let mut project_finders = get_finders();
        find_project_dirs(&repo, &mut project_finders, &config, remote).await?;

        Ok(Self {
            repo_root_path,
            config,
            project_finders,
        })
    }

    /// # Errors
    /// Returns error if retrieving the current directory fails.
    pub fn current_dir() -> Result<PathBuf> {
        Ok(std::env::current_dir()?)
    }
}
