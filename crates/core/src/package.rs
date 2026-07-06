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
    ///
    /// Excluded from coverage: tarpaulin mis-attributes the multi-line
    /// `&&`-condition's first line under normal rustfmt despite both
    /// branches being exercised by `test_check_changed_*`. The function
    /// is fully covered by its tests; the gap is a reporting artifact.
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
    fn language(&self) -> Language;

    fn dependencies(&self) -> &HashSet<String>;
    fn add_dependency(&mut self, dependency: &str);

    fn set_changed(&mut self, changed: bool);

    /// Set the package name (used for fallback when name is not found in manifest)
    fn set_name(&mut self, _name: String) {}

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

    /// Directories to prepend to `PATH` when running the publish / dry-run
    /// command for this package.
    ///
    /// Defaults to empty. The Node implementation returns the ancestor
    /// `node_modules/.bin` directories so lifecycle scripts (e.g. `husky` in a
    /// `prepare` hook) resolve during `bun publish` / `npm publish`, working
    /// around bun not adding them itself (oven-sh/bun#16071, #18055, #23594).
    fn publish_path_dirs(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
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
    #[cfg(not(tarpaulin_include))]
    async fn publish(&self, config: &Config) -> Result<crate::publish::PublishOutput> {
        let command = self.get_publish_command(config);
        crate::publish::run_publish_flow(
            &command,
            self.path(),
            &self.publish_path_dirs(),
            "Package directory not found",
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
            "Package directory not found",
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
    use rstest::rstest;
    use std::collections::HashMap;
    use std::path::PathBuf;

    macro_rules! impl_test_publish_commands {
        () => {
            fn default_publish_command(&self) -> String {
                "echo publish".to_string()
            }

            fn default_dry_run_publish_command(&self) -> Option<String> {
                Some("echo publish --dry-run".to_string())
            }
        };
    }

    #[derive(Debug)]
    struct MockPackage {
        name: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        version: Option<String>,
        language: Language,
        dependencies: HashSet<String>,
        is_changed: bool,
    }

    impl MockPackage {
        fn new(name: Option<&str>, path: &str, relative_path: &str) -> Self {
            Self {
                name: name.map(String::from),
                path: PathBuf::from(path),
                relative_path: PathBuf::from(relative_path),
                version: Some("1.0.0".to_string()),
                language: Language::Node,
                dependencies: HashSet::new(),
                is_changed: false,
            }
        }

        fn with_language(mut self, language: Language) -> Self {
            self.language = language;
            self
        }
    }

    #[async_trait]
    impl Package for MockPackage {
        // Consumes the same `impl_basic_accessors!()` macro that every
        // real-world `Package` impl uses (Node, Python, Rust, Dart,
        // CSharp, Java — 12 impls). This mock exists to prove the
        // macro's field-name contract survives future edits: if
        // someone renames a struct field (e.g. `is_changed` →
        // `changed`), these tests fail to compile immediately. The
        // struct fields above are pinned to the macro's expected
        // spellings (`name: Option<String>`, `version: Option<String>`,
        // `path: PathBuf`, `relative_path: PathBuf`, `is_changed: bool`).
        crate::impl_basic_accessors!();

        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            Ok(())
        }
        fn language(&self) -> Language {
            self.language
        }
        fn dependencies(&self) -> &HashSet<String> {
            &self.dependencies
        }
        fn add_dependency(&mut self, dependency: &str) {
            self.dependencies.insert(dependency.to_string());
        }
        impl_test_publish_commands!();
    }

    #[test]
    fn test_check_changed_already_changed() {
        let mut package = MockPackage::new(Some("test"), "/project/package.json", "package.json");
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
        let mut package = MockPackage::new(Some("test"), "/project/package.json", "package.json");
        package.check_changed(Path::new(changed_path)).unwrap();
        assert_eq!(package.is_changed(), expected);
    }

    #[test]
    fn test_inherits_workspace_version_default() {
        let package = MockPackage::new(Some("test"), "/project/package.json", "package.json");
        assert!(!package.inherits_workspace_version());
    }

    #[test]
    fn test_workspace_root_path_default() {
        let package = MockPackage::new(Some("test"), "/project/package.json", "package.json");
        assert!(package.workspace_root_path().is_none());
    }

    #[test]
    fn test_get_publish_command_by_path() {
        let package = MockPackage::new(
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
        let package =
            MockPackage::new(Some("test"), "/project/manifest", "manifest").with_language(language);
        let mut publish = HashMap::new();
        publish.insert(key.to_string(), command.to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(package.get_publish_command(&config), command);
    }

    #[test]
    fn test_get_publish_command_default() {
        let package = MockPackage::new(Some("test"), "/project/package.json", "package.json");
        let config = Config::default();

        assert_eq!(package.get_publish_command(&config), "echo publish");
    }

    #[tokio::test]
    async fn test_publish_success() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let package = MockPackage::new(Some("test"), path.to_str().unwrap(), "package.json");
        let config = Config::default();

        let output = package.publish(&config).await.unwrap();
        assert!(output.success);
    }

    #[tokio::test]
    async fn test_publish_failure() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let package = MockPackage::new(Some("test"), path.to_str().unwrap(), "package.json");
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

        let output = package.publish(&config).await.unwrap();
        assert!(!output.success);
    }

    #[tokio::test]
    async fn test_publish_no_parent_directory() {
        let package = MockPackage {
            name: Some("test".to_string()),
            path: PathBuf::from(""),
            relative_path: PathBuf::from(""),
            version: Some("1.0.0".to_string()),
            language: Language::Node,
            dependencies: HashSet::new(),
            is_changed: false,
        };
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

    #[test]
    fn test_set_name_updates_via_impl_basic_accessors_macro() {
        // Regression guard for item 10: MockPackage's `Package` impl uses
        // the shared `crate::impl_basic_accessors!()` macro, so `set_name`
        // MUST update the underlying `name` field (not fall through to the
        // trait's default no-op). If the macro's field-name contract
        // silently regresses (say, someone renames `name` on the mock and
        // the macro loses sight of it), the mock will fail to compile;
        // this test then locks the runtime behavior after compilation.
        let mut package =
            MockPackage::new(Some("original"), "/project/package.json", "package.json");
        package.set_name("new-name".to_string());
        assert_eq!(package.name(), Some("new-name"));
    }
}
