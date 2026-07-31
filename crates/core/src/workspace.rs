use std::{collections::HashSet, path::Path};

use crate::{Language, Package, update_type::UpdateType};
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

    crate::impl_shared_project_defaults!();

    crate::impl_publish_flows!(crate::publish::WORKSPACE_DIR_NOT_FOUND);

    crate::impl_publish_command_resolvers!();

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
    use crate::Config;
    use crate::test_support::MockWorkspace;
    use std::collections::BTreeMap;

    // The seventeen tests pinning the shared trait defaults are generated from
    // one surface shared with `package.rs`; only the `Workspace`-only defaults
    // below stay hand-written here.
    crate::test_support::shared_project_default_tests!(
        mock: MockWorkspace,
        trait_name: Workspace,
        kind: "workspace",
        dir_not_found: "Workspace directory not found",
        publishable_test: test_workspace_is_publishable_by_default,
    );

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
    async fn test_update_workspace_dependencies_default() {
        let workspace =
            MockWorkspace::with_paths(Some("test"), "/project/package.json", "package.json");
        let packages: Vec<&dyn Package> = vec![];

        let result = workspace.update_workspace_dependencies(&packages).await;
        assert!(result.is_ok());
    }
}
