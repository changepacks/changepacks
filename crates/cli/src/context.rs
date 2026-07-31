use crate::finders::get_finders;
use anyhow::{Context, Result};
use changepacks_core::Config;
use changepacks_core::ProjectFinder;
use changepacks_utils::{find_current_git_repo, find_project_dirs, get_changepacks_config_at};
use std::path::{Path, PathBuf};

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
    pub async fn new(remote: bool) -> Result<Self> {
        let current_dir =
            std::env::current_dir().context("Failed to determine current working directory")?;
        Self::new_at(&current_dir, remote).await
    }

    /// Builds a command context from a specific working directory.
    ///
    /// # Errors
    /// Returns error if finding the git repository, loading configuration, or
    /// discovering projects fails.
    pub async fn new_at(current_dir: &Path, remote: bool) -> Result<Self> {
        let repo = find_current_git_repo(current_dir)?;
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
            changepacks_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_utils::test_support::{git_add_and_commit, init_git_repo, run_git};
    use tempfile::TempDir;
    use tokio::fs;

    async fn write_repository(path: &std::path::Path) {
        init_git_repo(path);
        fs::create_dir_all(path.join(".changepacks")).await.unwrap();
        fs::write(
            path.join(".changepacks/config.json"),
            r#"{
                "ignore": ["examples/**"],
                "baseBranch": "main",
                "publish": { "node": "custom publish" }
            }"#,
        )
        .await
        .unwrap();
        fs::write(
            path.join("package.json"),
            r#"{"name":"fixture","version":"1.0.0"}"#,
        )
        .await
        .unwrap();
        git_add_and_commit(path, "initial");
    }

    #[tokio::test]
    async fn new_at_discovers_local_repository_and_loads_config() {
        let repository = TempDir::new().unwrap();
        let root = repository.path().canonicalize().unwrap();
        write_repository(&root).await;
        let nested = root.join("nested/directory");
        fs::create_dir_all(&nested).await.unwrap();

        let context = CommandContext::new_at(&nested, false).await.unwrap();

        assert_eq!(context.repo_root_path.canonicalize().unwrap(), root);
        assert_eq!(
            context.changepacks_dir.canonicalize().unwrap(),
            root.join(".changepacks").canonicalize().unwrap()
        );
        assert_eq!(context.config.ignore, ["examples/**"]);
        assert_eq!(context.config.base_branch, "main");
        assert_eq!(
            context.config.publish.get("node").map(String::as_str),
            Some("custom publish")
        );
        // Asserted through the production collector itself, so this covers both
        // the discovered-project count and the buffer `collect_projects` sizes
        // from it — strictly stronger than checking the capacity hint alone.
        assert_eq!(
            crate::finders::collect_projects(&context.project_finders).len(),
            1
        );
    }

    #[tokio::test]
    async fn new_at_supports_remote_base_branch_discovery() {
        let upstream = TempDir::new().unwrap();
        write_repository(upstream.path()).await;

        let clone = TempDir::new().unwrap();
        run_git(
            clone.path(),
            &["clone", upstream.path().to_str().unwrap(), "."],
        );
        run_git(clone.path(), &["config", "user.email", "test@test.com"]);
        run_git(clone.path(), &["config", "user.name", "Test"]);
        run_git(clone.path(), &["checkout", "-b", "feature"]);

        let context = CommandContext::new_at(clone.path(), true).await.unwrap();

        assert_eq!(
            context.repo_root_path.canonicalize().unwrap(),
            clone.path().canonicalize().unwrap()
        );
        assert_eq!(
            context.changepacks_dir.canonicalize().unwrap(),
            clone.path().join(".changepacks").canonicalize().unwrap()
        );
        assert_eq!(context.config.base_branch, "main");
    }

    #[tokio::test]
    async fn new_at_reports_missing_repository() {
        let directory = TempDir::new().unwrap();

        let error = CommandContext::new_at(directory.path(), false)
            .await
            .err()
            .expect("a directory outside a git repository must fail");

        assert!(
            error
                .to_string()
                .contains("Failed to discover git repository")
        );
    }
}
