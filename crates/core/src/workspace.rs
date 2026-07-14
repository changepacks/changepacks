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

    /// Set the workspace name (used for fallback when name is not found in manifest).
    /// Implementors typically get this via `impl_basic_accessors!()`.
    fn set_name(&mut self, name: String);

    /// Get the default publish command for this workspace type
    fn default_publish_command(&self) -> String;

    /// Get the default dry-run publish command for this workspace type.
    ///
    /// Returns `None` for ecosystems whose default publish tool does not
    /// support a built-in dry-run mode. Users may still provide an override
    /// via `config.publish_dry_run`.
    fn default_dry_run_publish_command(&self) -> Option<String>;

    /// Whether this project should be included in publish runs when no
    /// project-path or language command override is configured.
    fn is_publishable_by_default(&self) -> bool {
        true
    }

    /// Publish the workspace using the configured command or default
    ///
    /// # Errors
    /// Returns error if the publish command fails to spawn or the workspace directory is missing.
    /// A non-zero exit code is reported via `PublishOutput::success = false`.
    async fn publish(&self, config: &Config) -> Result<crate::publish::PublishOutput> {
        let command = self.get_publish_command(config);
        crate::publish::run_publish_flow(
            &command,
            self.path(),
            &[],
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
    async fn dry_run_publish(
        &self,
        config: &Config,
    ) -> Result<Option<crate::publish::PublishOutput>> {
        let command = self.get_dry_run_publish_command(config);
        crate::publish::run_dry_run_publish_flow(
            command.as_deref(),
            self.path(),
            &[],
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
    async fn update_workspace_dependencies(&self, _packages: &[&dyn Package]) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockWorkspace;
    use rstest::rstest;
    use std::{
        collections::{BTreeMap, HashSet},
        path::PathBuf,
    };

    #[derive(Debug)]
    struct UnsupportedDryRunWorkspace {
        path: PathBuf,
        dependencies: HashSet<String>,
    }

    #[async_trait]
    impl Workspace for UnsupportedDryRunWorkspace {
        fn name(&self) -> Option<&str> {
            Some("unsupported-dry-run")
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn relative_path(&self) -> &Path {
            Path::new("project.csproj")
        }

        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }

        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            Ok(())
        }

        fn language(&self) -> Language {
            Language::CSharp
        }

        fn dependencies(&self) -> &HashSet<String> {
            &self.dependencies
        }

        fn add_dependency(&mut self, dependency: &str) {
            self.dependencies.insert(dependency.to_string());
        }

        fn is_changed(&self) -> bool {
            false
        }

        fn set_changed(&mut self, _changed: bool) {}

        fn set_name(&mut self, _name: String) {}

        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }

        fn default_dry_run_publish_command(&self) -> Option<String> {
            None
        }
    }

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
    fn test_workspace_is_publishable_by_default() {
        let workspace =
            MockWorkspace::with_paths(Some("test"), "/project/package.json", "package.json");

        assert!(workspace.is_publishable_by_default());
    }

    #[test]
    fn test_get_publish_command_by_path() {
        let workspace = MockWorkspace::with_paths(
            Some("test"),
            "/project/package.json",
            "packages/core/package.json",
        );
        let mut publish = BTreeMap::new();
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
        let mut publish = BTreeMap::new();
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
        let mut publish_dry_run = BTreeMap::new();
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
        let mut publish_dry_run = BTreeMap::new();
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
        assert!(output.stdout.contains("publish"));
    }

    #[tokio::test]
    async fn test_publish_uses_project_path_override() {
        let path = std::env::temp_dir().join("package.json");
        let workspace = MockWorkspace::with_paths(
            Some("test"),
            path.to_str().unwrap(),
            "packages/core/package.json",
        );
        let config = Config {
            publish: BTreeMap::from([(
                "packages/core/package.json".to_string(),
                "echo workspace-path-override".to_string(),
            )]),
            ..Default::default()
        };

        let output = workspace.publish(&config).await.unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("workspace-path-override"));
    }

    #[tokio::test]
    async fn test_publish_uses_language_override() {
        let path = std::env::temp_dir().join("package.json");
        let workspace =
            MockWorkspace::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
        let config = Config {
            publish: BTreeMap::from([(
                "node".to_string(),
                "echo workspace-language-override".to_string(),
            )]),
            ..Default::default()
        };

        let output = workspace.publish(&config).await.unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("workspace-language-override"));
    }

    #[tokio::test]
    async fn test_publish_failure() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let workspace =
            MockWorkspace::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
        let mut publish = BTreeMap::new();
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

    #[tokio::test]
    async fn test_publish_reports_missing_current_directory() {
        let missing_dir = std::env::temp_dir().join(format!(
            "changepacks_missing_workspace_dir_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing_dir);
        let path = missing_dir.join("package.json");
        let workspace =
            MockWorkspace::with_paths(Some("test"), path.to_str().unwrap(), "package.json");

        let result = workspace.publish(&Config::default()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dry_run_publish_uses_project_path_override() {
        let path = std::env::temp_dir().join("package.json");
        let workspace = MockWorkspace::with_paths(
            Some("test"),
            path.to_str().unwrap(),
            "packages/core/package.json",
        );
        let config = Config {
            publish_dry_run: BTreeMap::from([(
                "packages/core/package.json".to_string(),
                "echo workspace-dry-path-override".to_string(),
            )]),
            ..Default::default()
        };

        let output = workspace.dry_run_publish(&config).await.unwrap().unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("workspace-dry-path-override"));
    }

    #[tokio::test]
    async fn test_dry_run_publish_uses_language_override() {
        let path = std::env::temp_dir().join("package.json");
        let workspace =
            MockWorkspace::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
        let config = Config {
            publish_dry_run: BTreeMap::from([(
                "node".to_string(),
                "echo workspace-dry-language-override".to_string(),
            )]),
            ..Default::default()
        };

        let output = workspace.dry_run_publish(&config).await.unwrap().unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("workspace-dry-language-override"));
    }

    #[tokio::test]
    async fn test_dry_run_publish_uses_default_command() {
        let path = std::env::temp_dir().join("package.json");
        let workspace =
            MockWorkspace::with_paths(Some("test"), path.to_str().unwrap(), "package.json");

        let output = workspace
            .dry_run_publish(&Config::default())
            .await
            .unwrap()
            .unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("publish --dry-run"));
    }

    #[tokio::test]
    async fn test_dry_run_publish_returns_none_when_unsupported() {
        let workspace = UnsupportedDryRunWorkspace {
            path: std::env::temp_dir().join("project.csproj"),
            dependencies: HashSet::new(),
        };

        let output = workspace.dry_run_publish(&Config::default()).await.unwrap();

        assert!(output.is_none());
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
