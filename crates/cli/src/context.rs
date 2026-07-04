use crate::finders::get_finders;
use anyhow::{Context, Result};
use changepacks_core::Config;
use changepacks_core::ProjectFinder;
use changepacks_utils::{
    ThreadSafeRepository, find_current_git_repo, find_project_dirs, get_changepacks_config,
};
use std::path::PathBuf;

/// Shared setup context for all CLI commands.
///
/// Contains git repository path, loaded config, and initialized project finders.
/// Instantiated once per command to avoid repetitive setup code.
pub struct CommandContext {
    /// Root path of the git repository
    pub repo_root_path: PathBuf,
    /// Loaded configuration from `.changepacks/config.json`
    pub config: Config,
    /// Project finders for all supported languages
    pub project_finders: Vec<Box<dyn ProjectFinder>>,
    /// Cached git repository handle so downstream commands (e.g. `update`)
    /// do not re-run `gix::discover` per invocation. `ThreadSafeRepository`
    /// is an internal `Arc` handle so this adds no measurable memory cost.
    pub repo: ThreadSafeRepository,
}

impl CommandContext {
    /// # Errors
    /// Returns error if finding git repository or discovering projects fails.
    ///
    /// Excluded from coverage: requires a real git repository and
    /// `find_project_dirs` walks the working tree; exercised end-to-end by
    /// the cli integration tests which already have full coverage of the
    /// surrounding command flow.
    #[cfg(not(tarpaulin_include))]
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
            repo,
        })
    }

    /// # Errors
    /// Returns error if retrieving the current directory fails.
    pub fn current_dir() -> Result<PathBuf> {
        Ok(std::env::current_dir()?)
    }

    /// Path to the `.changepacks/` directory at the repo root.
    ///
    /// Cached derivative of `repo_root_path` so downstream commands do not
    /// re-run `gix::discover` per invocation. `CommandContext::new` already
    /// sets `repo_root_path = repo.work_dir()`, which is exactly what
    /// `changepacks_utils::get_changepacks_dir(current_dir)` computes, so
    /// this is byte-identical to the previous
    /// `get_changepacks_dir(&CommandContext::current_dir()?)?` pattern.
    #[must_use]
    pub fn changepacks_dir(&self) -> PathBuf {
        self.repo_root_path.join(".changepacks")
    }
}
