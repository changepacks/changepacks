use std::{collections::HashSet, path::Path};

use crate::{
    Config, Language, Package, change_detection::should_mark_changed, update_type::UpdateType,
};
use anyhow::Result;
use async_trait::async_trait;

/// Interface for monorepo workspace roots.
///
/// Extends Package behavior with workspace-specific operations like updating workspace
/// dependencies. Implemented by language-specific workspace types.
#[async_trait]
pub trait Workspace: std::fmt::Debug + Send + Sync {
    fn name(&self) -> Option<&str>;
    fn path(&self) -> &Path;
    fn relative_path(&self) -> &Path;
    fn version(&self) -> Option<&str>;
    /// # Errors
    /// Returns error if the version update operation fails.
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()>;
    fn language(&self) -> Language;

    fn dependencies(&self) -> &HashSet<String>;
    fn add_dependency(&mut self, dependency: &str);

    /// # Errors
    /// Returns error if the parent path cannot be determined.
    // Default implementation for check_changed
    ///
    /// Excluded from coverage: see `Package::check_changed` for the same
    /// tarpaulin attribution caveat on the multi-line `&&` condition.
    #[cfg(not(tarpaulin_include))]
    fn check_changed(&mut self, path: &Path) -> Result<()> {
        if self.is_changed() {
            return Ok(());
        }
        if should_mark_changed(path, self.path())? {
            self.set_changed(true);
        }
        Ok(())
    }

    fn is_changed(&self) -> bool;
    fn set_changed(&mut self, changed: bool);

    /// Set the workspace name (used for fallback when name is not found in manifest)
    fn set_name(&mut self, _name: String) {}

    /// Get the default publish command for this workspace type
    fn default_publish_command(&self) -> String;

    /// Get the default dry-run publish command for this workspace type.
    ///
    /// Returns `None` for ecosystems whose default publish tool does not
    /// support a built-in dry-run mode. Users may still provide an override
    /// via `config.publish_dry_run`.
    fn default_dry_run_publish_command(&self) -> Option<String>;

    /// Directories to prepend to `PATH` when running the publish / dry-run
    /// command for this workspace.
    ///
    /// Defaults to empty. The Node implementation returns the ancestor
    /// `node_modules/.bin` directories so lifecycle scripts (e.g. `husky` in a
    /// `prepare` hook) resolve during `bun publish` / `npm publish`, working
    /// around bun not adding them itself (oven-sh/bun#16071, #18055, #23594).
    fn publish_path_dirs(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    /// Publish the workspace using the configured command or default
    ///
    /// # Errors
    /// Returns error if the publish command fails to spawn or the workspace directory is missing.
    /// A non-zero exit code is reported via `PublishOutput::success = false`.
    #[cfg(not(tarpaulin_include))]
    async fn publish(&self, config: &Config) -> Result<crate::publish::PublishOutput> {
        let command = self.get_publish_command(config);
        crate::publish::run_publish_flow(
            &command,
            self.path(),
            &self.publish_path_dirs(),
            crate::publish::WORKSPACE_DIR_NOT_FOUND,
        )
        .await
    }

    /// Run the publish command in dry-run mode to verify the pre-release flow
    /// works without actually publishing.
    ///
    /// Returns `Ok(Some(output))` with the captured command output, or
    /// `Ok(None)` when the language does not support a dry-run mode and the
    /// user has not provided an override in `config.publish_dry_run`.
    ///
    /// # Errors
    /// Returns error if the dry-run command fails to spawn or the workspace
    /// directory is missing. A non-zero exit code is reported via
    /// `PublishOutput::success = false`.
    #[cfg(not(tarpaulin_include))]
    async fn dry_run_publish(
        &self,
        config: &Config,
    ) -> Result<Option<crate::publish::PublishOutput>> {
        let command = self.get_dry_run_publish_command(config);
        crate::publish::run_dry_run_publish_flow(
            command.as_deref(),
            self.path(),
            &self.publish_path_dirs(),
            crate::publish::WORKSPACE_DIR_NOT_FOUND,
        )
        .await
    }

    /// Get the publish command for this workspace, checking config first.
    ///
    /// The `default_publish_command()` closure is `FnOnce`, so the
    /// workspace's language-specific default (e.g. Node's
    /// `detect_package_manager_recursive`, which walks the ancestor chain
    /// with sync filesystem stats) is only invoked when config supplies
    /// neither a per-path nor a per-language override — the common case
    /// where the user configures a custom publish command in
    /// `.changepacks/config.json` now avoids one `String` allocation and,
    /// for Node, the ancestor-walking probe.
    fn get_publish_command(&self, config: &Config) -> String {
        crate::publish::resolve_publish_command(
            self.relative_path(),
            self.language(),
            || self.default_publish_command(),
            config,
        )
    }

    /// Get the dry-run publish command for this workspace, checking config
    /// first, then falling back to the workspace's `default_dry_run_publish_command`.
    ///
    /// Mirrors [`Workspace::get_publish_command`] — the default closure is
    /// `FnOnce` so it is only invoked on the cache-miss path.
    fn get_dry_run_publish_command(&self, config: &Config) -> Option<String> {
        crate::publish::resolve_dry_run_publish_command(
            self.relative_path(),
            self.language(),
            || self.default_dry_run_publish_command(),
            config,
        )
    }

    /// Updates workspace-level dependency versions after package versions are bumped.
    ///
    /// This is an intentional no-op in the default implementation. Only `RustWorkspace`
    /// overrides this method to sync `[workspace.dependencies]` path-dependency versions
    /// with their corresponding package versions.
    ///
    /// # Errors
    ///
    /// Returns an error only if a language override's dependency rewrite fails.
    #[cfg(not(tarpaulin_include))]
    async fn update_workspace_dependencies(&self, _packages: &[&dyn Package]) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockWorkspace;
    use rstest::rstest;
    use std::collections::HashMap;

    #[test]
    fn test_check_changed_already_changed() {
        let mut workspace =
            MockWorkspace::with_paths(Some("test"), "/project/package.json", "package.json");
        workspace.is_changed = true;

        // Should return early if already changed
        workspace
            .check_changed(Path::new("/project/src/index.js"))
            .unwrap();
        assert!(workspace.is_changed());
    }

    #[rstest]
    // A file inside the project dir marks it changed; a changepack log or a
    // file that belongs to another project does not.
    #[case("/project/src/index.js", true)]
    #[case("/project/.changepacks/change.json", false)]
    #[case("/other-project/src/index.js", false)]
    fn test_check_changed(#[case] changed_path: &str, #[case] expected: bool) {
        let mut workspace =
            MockWorkspace::with_paths(Some("test"), "/project/package.json", "package.json");
        workspace.check_changed(Path::new(changed_path)).unwrap();
        assert_eq!(workspace.is_changed(), expected);
    }

    #[test]
    fn test_get_publish_command_by_path() {
        let workspace = MockWorkspace::with_paths(
            Some("test"),
            "/project/package.json",
            "packages/core/package.json",
        );
        let mut publish = HashMap::new();
        publish.insert(
            "packages/core/package.json".to_string(),
            "custom publish".to_string(),
        );
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(workspace.get_publish_command(&config), "custom publish");
    }

    #[rstest]
    #[case(Language::Node, "node", "npm publish --access public")]
    #[case(Language::Python, "python", "poetry publish")]
    #[case(Language::Rust, "rust", "cargo publish")]
    #[case(Language::Dart, "dart", "dart pub publish")]
    fn test_get_publish_command_by_language(
        #[case] language: Language,
        #[case] key: &str,
        #[case] command: &str,
    ) {
        let workspace = MockWorkspace::with_paths(Some("test"), "/project/manifest", "manifest")
            .with_language(language);
        let mut publish = HashMap::new();
        publish.insert(key.to_string(), command.to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(workspace.get_publish_command(&config), command);
    }

    #[test]
    fn test_get_publish_command_default() {
        let workspace =
            MockWorkspace::with_paths(Some("test"), "/project/package.json", "package.json");
        let config = Config::default();

        assert_eq!(workspace.get_publish_command(&config), "echo publish");
    }

    #[test]
    fn test_get_dry_run_publish_command_falls_back_to_workspace_default() {
        let workspace =
            MockWorkspace::with_paths(Some("test"), "/project/package.json", "package.json")
                .with_language(Language::Node);
        let config = Config::default();

        // With no override, the trait method returns the workspace's own
        // `default_dry_run_publish_command()` (here, the MockWorkspace stub).
        assert_eq!(
            workspace.get_dry_run_publish_command(&config).as_deref(),
            Some("echo publish --dry-run")
        );
    }

    #[test]
    fn test_get_dry_run_publish_command_override_by_path() {
        let workspace = MockWorkspace::with_paths(
            Some("test"),
            "/project/package.json",
            "packages/core/package.json",
        );
        let mut publish_dry_run = HashMap::new();
        publish_dry_run.insert(
            "packages/core/package.json".to_string(),
            "custom dry".to_string(),
        );
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        // Per-project override wins over the workspace's own default.
        assert_eq!(
            workspace.get_dry_run_publish_command(&config).as_deref(),
            Some("custom dry")
        );
    }

    #[test]
    fn test_get_dry_run_publish_command_override_by_language() {
        let workspace =
            MockWorkspace::with_paths(Some("test"), "/project/package.json", "package.json")
                .with_language(Language::Node);
        let mut publish_dry_run = HashMap::new();
        publish_dry_run.insert(
            "node".to_string(),
            "npm publish --dry-run --tag next".to_string(),
        );
        let config = Config {
            publish_dry_run,
            ..Default::default()
        };

        // Per-language override wins over the workspace's own default.
        assert_eq!(
            workspace.get_dry_run_publish_command(&config).as_deref(),
            Some("npm publish --dry-run --tag next")
        );
    }

    #[tokio::test]
    async fn test_publish_success() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let workspace =
            MockWorkspace::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
        let config = Config::default();

        // This will run "echo publish" which should succeed
        let output = workspace.publish(&config).await.unwrap();
        assert!(output.success);
    }

    #[tokio::test]
    async fn test_publish_failure() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let workspace =
            MockWorkspace::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
        let mut publish = HashMap::new();
        let fail_cmd = if cfg!(target_os = "windows") {
            "cmd /c exit 1"
        } else {
            "exit 1"
        };
        publish.insert("node".to_string(), fail_cmd.to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        let output = workspace.publish(&config).await.unwrap();
        assert!(!output.success);
    }

    #[tokio::test]
    async fn test_update_workspace_dependencies_default() {
        let workspace =
            MockWorkspace::with_paths(Some("test"), "/project/package.json", "package.json");
        let packages: Vec<&dyn Package> = vec![];

        let result = workspace.update_workspace_dependencies(&packages).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_publish_no_parent_directory() {
        let workspace = MockWorkspace::with_paths(Some("test"), "", "");
        let config = Config::default();
        let result = workspace.publish(&config).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Workspace directory not found")
        );
    }

    #[test]
    fn test_set_name_updates_via_impl_basic_accessors_macro() {
        // Regression guard for the shared-macro accessor contract — see the
        // sibling test in
        // `package.rs::tests::test_set_name_updates_via_impl_basic_accessors_macro`
        // for the full rationale. The mock's `Workspace` impl uses
        // `crate::impl_basic_accessors!()`, so `set_name` MUST update the
        // underlying `name` field.
        let mut workspace =
            MockWorkspace::with_paths(Some("original"), "/project/package.json", "package.json");
        workspace.set_name("new-name".to_string());
        assert_eq!(workspace.name(), Some("new-name"));
    }
}
