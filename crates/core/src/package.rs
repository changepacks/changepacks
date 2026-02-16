use std::{collections::HashSet, path::Path};

use crate::{Config, Language, update_type::UpdateType};
use anyhow::{Context, Result};
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
        if !path.to_string_lossy().contains(".changepacks")
            && path.starts_with(self.path().parent().context("Parent not found")?)
        {
            self.set_changed(true);
        }
        Ok(())
    }
    fn is_changed(&self) -> bool;
    fn language(&self) -> Language;

    fn dependencies(&self) -> &HashSet<String>;
    fn add_dependency(&mut self, dependency: &str);

    fn set_changed(&mut self, changed: bool);

    /// Get the default publish command for this package type
    fn default_publish_command(&self) -> String;

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
    /// Returns error if the publish command fails to execute or returns non-zero exit code.
    async fn publish(&self, config: &Config) -> Result<()> {
        let command = self.get_publish_command(config);
        let dir = self
            .path()
            .parent()
            .context("Package directory not found")?;
        crate::publish::run_publish_command(&command, dir).await
    }

    /// Get the publish command for this package, checking config first
    fn get_publish_command(&self, config: &Config) -> String {
        crate::publish::resolve_publish_command(
            self.relative_path(),
            self.language(),
            &self.default_publish_command(),
            config,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[derive(Debug)]
    struct MockPackage {
        name: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        version: Option<String>,
        language: Language,
        dependencies: HashSet<String>,
        changed: bool,
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
                changed: false,
            }
        }

        fn with_language(mut self, language: Language) -> Self {
            self.language = language;
            self
        }
    }

    #[async_trait]
    impl Package for MockPackage {
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }
        fn version(&self) -> Option<&str> {
            self.version.as_deref()
        }
        fn path(&self) -> &Path {
            &self.path
        }
        fn relative_path(&self) -> &Path {
            &self.relative_path
        }
        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            Ok(())
        }
        fn is_changed(&self) -> bool {
            self.changed
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
        fn set_changed(&mut self, changed: bool) {
            self.changed = changed;
        }
        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
    }

    #[test]
    fn test_check_changed_already_changed() {
        let mut package = MockPackage::new(Some("test"), "/project/package.json", "package.json");
        package.changed = true;

        package
            .check_changed(Path::new("/project/src/index.js"))
            .unwrap();
        assert!(package.is_changed());
    }

    #[test]
    fn test_check_changed_sets_changed() {
        let mut package = MockPackage::new(Some("test"), "/project/package.json", "package.json");

        package
            .check_changed(Path::new("/project/src/index.js"))
            .unwrap();
        assert!(package.is_changed());
    }

    #[test]
    fn test_check_changed_ignores_changepacks() {
        let mut package = MockPackage::new(Some("test"), "/project/package.json", "package.json");

        package
            .check_changed(Path::new("/project/.changepacks/change.json"))
            .unwrap();
        assert!(!package.is_changed());
    }

    #[test]
    fn test_check_changed_ignores_other_projects() {
        let mut package = MockPackage::new(Some("test"), "/project/package.json", "package.json");

        package
            .check_changed(Path::new("/other-project/src/index.js"))
            .unwrap();
        assert!(!package.is_changed());
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

    #[test]
    fn test_get_publish_command_by_language_node() {
        let package = MockPackage::new(Some("test"), "/project/package.json", "package.json")
            .with_language(Language::Node);
        let mut publish = HashMap::new();
        publish.insert(
            "node".to_string(),
            "npm publish --access public".to_string(),
        );
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(
            package.get_publish_command(&config),
            "npm publish --access public"
        );
    }

    #[test]
    fn test_get_publish_command_by_language_python() {
        let package = MockPackage::new(Some("test"), "/project/pyproject.toml", "pyproject.toml")
            .with_language(Language::Python);
        let mut publish = HashMap::new();
        publish.insert("python".to_string(), "poetry publish".to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(package.get_publish_command(&config), "poetry publish");
    }

    #[test]
    fn test_get_publish_command_by_language_rust() {
        let package = MockPackage::new(Some("test"), "/project/Cargo.toml", "Cargo.toml")
            .with_language(Language::Rust);
        let mut publish = HashMap::new();
        publish.insert("rust".to_string(), "cargo publish".to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(package.get_publish_command(&config), "cargo publish");
    }

    #[test]
    fn test_get_publish_command_by_language_dart() {
        let package = MockPackage::new(Some("test"), "/project/pubspec.yaml", "pubspec.yaml")
            .with_language(Language::Dart);
        let mut publish = HashMap::new();
        publish.insert("dart".to_string(), "dart pub publish".to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(package.get_publish_command(&config), "dart pub publish");
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

        let result = package.publish(&config).await;
        assert!(result.is_ok());
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

        let result = package.publish(&config).await;
        assert!(result.is_err());
    }
}
