use std::{collections::HashSet, path::Path};

use crate::{Language, update_type::UpdateType};
use anyhow::Result;
use async_trait::async_trait;

/// Interface for single versioned packages.
///
/// Implemented by language-specific package types for reading versions, updating files,
/// detecting changes, and publishing. All I/O operations are async.
#[async_trait]
pub trait Package: std::fmt::Debug + Send + Sync {
    fn name(&self) -> Option<&str>;
    fn version(&self) -> Option<&str>;
    fn path(&self) -> &Path;
    fn relative_path(&self) -> &Path;
    /// # Errors
    /// Returns error if the version update operation fails.
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()>;
    fn is_changed(&self) -> bool;
    fn language(&self) -> Language;

    fn dependencies(&self) -> &HashSet<String>;
    fn add_dependency(&mut self, dependency: &str);

    fn set_changed(&mut self, changed: bool);

    /// Set the package name (used for fallback when name is not found in manifest).
    /// Implementors typically get this via `impl_basic_accessors!()`.
    fn set_name(&mut self, name: String);

    /// Get the default publish command for this package type
    fn default_publish_command(&self) -> String;

    /// Get the default dry-run publish command for this package type.
    ///
    /// Returns `None` for ecosystems whose default publish tool does not
    /// support a built-in dry-run mode (e.g. `dotnet nuget push`). Callers
    /// should treat `None` as "dry-run not supported; skip with a warning"
    /// rather than as a failure. Users may still provide an override via
    /// `config.publish_dry_run`.
    fn default_dry_run_publish_command(&self) -> Option<String>;

    crate::impl_shared_project_defaults!();

    /// Whether this package inherits its version from the workspace root via `version.workspace = true`
    fn inherits_workspace_version(&self) -> bool {
        false
    }

    /// Path to the workspace root Cargo.toml, if this package inherits its version from workspace
    fn workspace_root_path(&self) -> Option<&Path> {
        None
    }

    crate::impl_publish_flows!(crate::publish::PACKAGE_DIR_NOT_FOUND);

    crate::impl_publish_command_resolvers!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockPackage;

    // The seventeen tests pinning the shared trait defaults are generated from
    // one surface shared with `workspace.rs`; only the `Package`-only defaults
    // below stay hand-written here.
    crate::test_support::shared_project_default_tests!(
        mock: MockPackage,
        trait_name: Package,
        kind: "package",
        dir_not_found: "Package directory not found",
        publishable_test: test_package_is_publishable_by_default,
    );

    #[test]
    fn test_inherits_workspace_version_default() {
        let package =
            MockPackage::with_paths(Some("test"), "/project/package.json", "package.json");
        assert!(!package.inherits_workspace_version());
    }

    #[test]
    fn test_workspace_root_path_default() {
        let package =
            MockPackage::with_paths(Some("test"), "/project/package.json", "package.json");
        assert!(package.workspace_root_path().is_none());
    }
}
