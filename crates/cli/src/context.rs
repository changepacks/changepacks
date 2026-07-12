use crate::finders::get_finders;
use anyhow::{Context, Result};
use changepacks_core::Config;
use changepacks_core::ProjectFinder;
use changepacks_utils::{
    ThreadSafeRepository, find_current_git_repo, find_project_dirs, get_changepacks_config_at,
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
    /// Cached `<repo_root_path>/.changepacks` computed once in `new()` so
    /// every `check`/`update`/`changepack` command reads a stable
    /// `&PathBuf` (deref-coerces to `&Path`) instead of re-allocating a
    /// fresh `PathBuf` via `repo_root_path.join(".changepacks")` on every
    /// call. Byte-identical to `repo_root_path.join(".changepacks")`
    /// because `CommandContext::new` sets `repo_root_path =
    /// repo.work_dir()`, matching what `get_changepacks_dir(current_dir)`
    /// computes. Exposed as a `pub` field (matching every other
    /// `CommandContext` field) so callers like `update.rs` can borrow it
    /// with `&ctx.changepacks_dir` after partially moving other fields
    /// such as `ctx.project_finders` — a method-based accessor would
    /// require an outstanding `&self` borrow that conflicts with the
    /// partial-move pattern already in use downstream.
    pub changepacks_dir: PathBuf,
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
        let current_dir =
            std::env::current_dir().context("Failed to determine current working directory")?;
        let repo = find_current_git_repo(&current_dir)?;
        let repo_root_path = repo
            .work_dir()
            .context("Not a git working directory. Ensure you are inside a git repository.")?
            .to_path_buf();
        // Compute `changepacks_dir` up front so we can hand it to
        // `get_changepacks_config_at`, skipping a redundant second
        // `gix::discover(current_dir)` walk that the current-dir wrapper
        // performs via `get_changepacks_dir`. `repo.work_dir().join(...)`
        // is byte-identical to `get_changepacks_dir(current_dir)?` because
        // `find_current_git_repo` already yielded the same repo.
        let changepacks_dir = repo_root_path.join(".changepacks");
        let config = get_changepacks_config_at(&changepacks_dir).await?;
        let mut project_finders = get_finders();
        find_project_dirs(&repo, &mut project_finders, &config, remote).await?;

        Ok(Self {
            repo_root_path,
            config,
            project_finders,
            repo,
            changepacks_dir,
        })
    }
}
