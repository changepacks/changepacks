use std::{collections::HashSet, path::Path};

use crate::{Config, Language, change_detection::should_mark_changed, update_type::UpdateType};
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

    /// Whether this project should be included in publish runs when no
    /// project-path or language command override is configured.
    fn is_publishable_by_default(&self) -> bool {
        true
    }

    /// Whether this project should be included in dry-run publish runs when no
    /// project-path or language command override is configured.
    fn is_dry_run_publishable_by_default(&self) -> bool {
        self.is_publishable_by_default()
    }

    /// Whether this package inherits its version from the workspace root via `version.workspace = true`
    fn inherits_workspace_version(&self) -> bool {
        false
    }

    /// Path to the workspace root Cargo.toml, if this package inherits its version from workspace
    fn workspace_root_path(&self) -> Option<&Path> {
        None
    }

    /// Publish the package using the configured command or default
    ///
    /// # Errors
    /// Returns error if the publish command fails to spawn or the package directory is missing.
    /// A non-zero exit code is reported via `PublishOutput::success = false`.
    async fn publish(&self, config: &Config) -> Result<crate::publish::PublishOutput> {
        let command = self.get_publish_command(config);
        crate::publish::run_publish_flow(
            &command,
            self.path(),
            &[],
            crate::publish::PACKAGE_DIR_NOT_FOUND,
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
    /// Returns error if the dry-run command fails to spawn or the package
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
            crate::publish::PACKAGE_DIR_NOT_FOUND,
        )
        .await
    }

    /// Get the publish command for this package, checking config first.
    ///
    /// The `default_publish_command()` closure is `FnOnce`, so the
    /// package's language-specific default (e.g. Node's
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

    /// Get the dry-run publish command for this package, checking config
    /// first, then falling back to the package's `default_dry_run_publish_command`.
    ///
    /// Mirrors [`Package::get_publish_command`] — the default closure is
    /// `FnOnce` so it is only invoked on the cache-miss path.
    fn get_dry_run_publish_command(&self, config: &Config) -> Option<String> {
        crate::publish::resolve_dry_run_publish_command(
            self.relative_path(),
            self.language(),
            || self.default_dry_run_publish_command(),
            config,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockPackage, UnsupportedDryRunProject};
    use rstest::rstest;
    use std::collections::{BTreeMap, HashSet};

    #[test]
    fn test_check_changed_already_changed() {
        let mut package =
            MockPackage::with_paths(Some("test"), "/project/package.json", "package.json");
        package.is_changed = true;

        package
            .check_changed(Path::new("/project/src/index.js"))
            .unwrap();
        assert!(package.is_changed());
    }

    #[rstest]
    // A file inside the project dir marks it changed; a changepack log or a
    // file that belongs to another project does not.
    #[case("/project/src/index.js", true)]
    #[case("/project/.changepacks/change.json", false)]
    #[case("/other-project/src/index.js", false)]
    fn test_check_changed(#[case] changed_path: &str, #[case] expected: bool) {
        let mut package =
            MockPackage::with_paths(Some("test"), "/project/package.json", "package.json");
        package.check_changed(Path::new(changed_path)).unwrap();
        assert_eq!(package.is_changed(), expected);
    }

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

    #[test]
    fn test_package_is_publishable_by_default() {
        let package =
            MockPackage::with_paths(Some("test"), "/project/package.json", "package.json");

        assert!(package.is_publishable_by_default());
        assert_eq!(
            package.is_dry_run_publishable_by_default(),
            package.is_publishable_by_default()
        );
    }

    #[test]
    fn test_get_publish_command_by_path() {
        let package = MockPackage::with_paths(
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

        assert_eq!(package.get_publish_command(&config), "custom publish");
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
        let package = MockPackage::with_paths(Some("test"), "/project/manifest", "manifest")
            .with_language(language);
        let mut publish = BTreeMap::new();
        publish.insert(key.to_string(), command.to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(package.get_publish_command(&config), command);
    }

    #[test]
    fn test_get_publish_command_default() {
        let package =
            MockPackage::with_paths(Some("test"), "/project/package.json", "package.json");
        let config = Config::default();

        assert_eq!(package.get_publish_command(&config), "echo publish");
    }

    #[tokio::test]
    async fn test_publish_success() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let package = MockPackage::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
        let config = Config::default();

        let output = package.publish(&config).await.unwrap();
        assert!(output.success);
        assert!(output.stdout.contains("publish"));
    }

    #[tokio::test]
    async fn test_publish_uses_project_path_override() {
        let path = std::env::temp_dir().join("package.json");
        let package = MockPackage::with_paths(
            Some("test"),
            path.to_str().unwrap(),
            "packages/core/package.json",
        );
        let config = Config {
            publish: BTreeMap::from([(
                "packages/core/package.json".to_string(),
                "echo package-path-override".to_string(),
            )]),
            ..Default::default()
        };

        let output = package.publish(&config).await.unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("package-path-override"));
    }

    #[tokio::test]
    async fn test_publish_uses_language_override() {
        let path = std::env::temp_dir().join("package.json");
        let package = MockPackage::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
        let config = Config {
            publish: BTreeMap::from([(
                "node".to_string(),
                "echo package-language-override".to_string(),
            )]),
            ..Default::default()
        };

        let output = package.publish(&config).await.unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("package-language-override"));
    }

    #[tokio::test]
    async fn test_publish_failure() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let package = MockPackage::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
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

        let output = package.publish(&config).await.unwrap();
        assert!(!output.success);
    }

    #[tokio::test]
    async fn test_publish_no_parent_directory() {
        let package = MockPackage::with_paths(Some("test"), "", "");
        let config = Config::default();
        let result = package.publish(&config).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Package directory not found")
        );
    }

    #[tokio::test]
    async fn test_publish_reports_missing_current_directory() {
        let missing_dir = std::env::temp_dir().join(format!(
            "changepacks_missing_package_dir_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing_dir);
        let path = missing_dir.join("package.json");
        let package = MockPackage::with_paths(Some("test"), path.to_str().unwrap(), "package.json");

        let result = package.publish(&Config::default()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dry_run_publish_uses_project_path_override() {
        let path = std::env::temp_dir().join("package.json");
        let package = MockPackage::with_paths(
            Some("test"),
            path.to_str().unwrap(),
            "packages/core/package.json",
        );
        let config = Config {
            publish_dry_run: BTreeMap::from([(
                "packages/core/package.json".to_string(),
                "echo package-dry-path-override".to_string(),
            )]),
            ..Default::default()
        };

        let output = package.dry_run_publish(&config).await.unwrap().unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("package-dry-path-override"));
    }

    #[tokio::test]
    async fn test_dry_run_publish_uses_language_override() {
        let path = std::env::temp_dir().join("package.json");
        let package = MockPackage::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
        let config = Config {
            publish_dry_run: BTreeMap::from([(
                "node".to_string(),
                "echo package-dry-language-override".to_string(),
            )]),
            ..Default::default()
        };

        let output = package.dry_run_publish(&config).await.unwrap().unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("package-dry-language-override"));
    }

    #[tokio::test]
    async fn test_dry_run_publish_uses_default_command() {
        let path = std::env::temp_dir().join("package.json");
        let package = MockPackage::with_paths(Some("test"), path.to_str().unwrap(), "package.json");

        let output = package
            .dry_run_publish(&Config::default())
            .await
            .unwrap()
            .unwrap();

        assert!(output.success);
        assert!(output.stdout.contains("publish --dry-run"));
    }

    #[tokio::test]
    async fn test_dry_run_publish_returns_none_when_unsupported() {
        let package = UnsupportedDryRunProject {
            path: std::env::temp_dir().join("project.csproj"),
            dependencies: HashSet::new(),
        };

        let output = Package::dry_run_publish(&package, &Config::default())
            .await
            .unwrap();

        assert!(output.is_none());
    }

    #[test]
    fn test_set_name_updates_via_impl_basic_accessors_macro() {
        // Regression guard for the shared-macro accessor contract:
        // MockPackage's `Package` impl uses
        // the shared `crate::impl_basic_accessors!()` macro, so `set_name`
        // MUST update the underlying `name` field (not fall through to the
        // trait's default no-op). If the macro's field-name contract
        // silently regresses (say, someone renames `name` on the mock and
        // the macro loses sight of it), the mock will fail to compile;
        // this test then locks the runtime behavior after compilation.
        let mut package =
            MockPackage::with_paths(Some("original"), "/project/package.json", "package.json");
        package.set_name("new-name".to_string());
        assert_eq!(package.name(), Some("new-name"));
    }
}
