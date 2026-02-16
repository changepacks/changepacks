use std::{collections::HashSet, path::Path};

use crate::{Config, Language, Package, update_type::UpdateType};
use anyhow::{Context, Result};
use async_trait::async_trait;

#[async_trait]
pub trait Workspace: std::fmt::Debug + Send + Sync {
    fn name(&self) -> Option<&str>;
    fn path(&self) -> &Path;
    fn relative_path(&self) -> &Path;
    fn version(&self) -> Option<&str>;
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()>;
    fn language(&self) -> Language;

    fn dependencies(&self) -> &HashSet<String>;
    fn add_dependency(&mut self, dependency: &str);

    // Default implementation for check_changed
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
    fn set_changed(&mut self, changed: bool);

    /// Get the default publish command for this workspace type
    fn default_publish_command(&self) -> String;

    /// Publish the workspace using the configured command or default
    async fn publish(&self, config: &Config) -> Result<()> {
        let command = self.get_publish_command(config);
        // Get the directory containing the workspace file
        let workspace_dir = self
            .path()
            .parent()
            .context("Workspace directory not found")?;

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C");
            c.arg(command);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c");
            c.arg(command);
            c
        };

        cmd.current_dir(workspace_dir);
        let output = cmd.output().await?;

        if !output.status.success() {
            anyhow::bail!(
                "Publish command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    /// Get the publish command for this workspace, checking config first
    fn get_publish_command(&self, config: &Config) -> String {
        // Check for custom command by relative path
        if let Some(cmd) = config
            .publish
            .get(self.relative_path().to_string_lossy().as_ref())
        {
            return cmd.clone();
        }

        // Check for custom command by language
        let lang_key = match self.language() {
            crate::Language::Node => "node",
            crate::Language::Python => "python",
            crate::Language::Rust => "rust",
            crate::Language::Dart => "dart",
            crate::Language::CSharp => "csharp",
            crate::Language::Java => "java",
        };
        if let Some(cmd) = config.publish.get(lang_key) {
            return cmd.clone();
        }

        // Use default command
        self.default_publish_command()
    }

    async fn update_workspace_dependencies(&self, _packages: &[&dyn Package]) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[derive(Debug)]
    struct MockWorkspace {
        name: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        version: Option<String>,
        language: Language,
        dependencies: HashSet<String>,
        changed: bool,
    }

    impl MockWorkspace {
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
    impl Workspace for MockWorkspace {
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }
        fn path(&self) -> &Path {
            &self.path
        }
        fn relative_path(&self) -> &Path {
            &self.relative_path
        }
        fn version(&self) -> Option<&str> {
            self.version.as_deref()
        }
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
        fn is_changed(&self) -> bool {
            self.changed
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
        let mut workspace =
            MockWorkspace::new(Some("test"), "/project/package.json", "package.json");
        workspace.changed = true;

        // Should return early if already changed
        workspace
            .check_changed(Path::new("/project/src/index.js"))
            .unwrap();
        assert!(workspace.is_changed());
    }

    #[test]
    fn test_check_changed_sets_changed() {
        let mut workspace =
            MockWorkspace::new(Some("test"), "/project/package.json", "package.json");

        // File in project directory should mark as changed
        workspace
            .check_changed(Path::new("/project/src/index.js"))
            .unwrap();
        assert!(workspace.is_changed());
    }

    #[test]
    fn test_check_changed_ignores_changepacks() {
        let mut workspace =
            MockWorkspace::new(Some("test"), "/project/package.json", "package.json");

        // Files in .changepacks should be ignored
        workspace
            .check_changed(Path::new("/project/.changepacks/change.json"))
            .unwrap();
        assert!(!workspace.is_changed());
    }

    #[test]
    fn test_check_changed_ignores_other_projects() {
        let mut workspace =
            MockWorkspace::new(Some("test"), "/project/package.json", "package.json");

        // Files in other directories should not mark as changed
        workspace
            .check_changed(Path::new("/other-project/src/index.js"))
            .unwrap();
        assert!(!workspace.is_changed());
    }

    #[test]
    fn test_get_publish_command_by_path() {
        let workspace = MockWorkspace::new(
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

    #[test]
    fn test_get_publish_command_by_language() {
        let workspace = MockWorkspace::new(Some("test"), "/project/package.json", "package.json")
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
            workspace.get_publish_command(&config),
            "npm publish --access public"
        );
    }

    #[test]
    fn test_get_publish_command_python() {
        let workspace =
            MockWorkspace::new(Some("test"), "/project/pyproject.toml", "pyproject.toml")
                .with_language(Language::Python);
        let mut publish = HashMap::new();
        publish.insert("python".to_string(), "poetry publish".to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(workspace.get_publish_command(&config), "poetry publish");
    }

    #[test]
    fn test_get_publish_command_rust() {
        let workspace = MockWorkspace::new(Some("test"), "/project/Cargo.toml", "Cargo.toml")
            .with_language(Language::Rust);
        let mut publish = HashMap::new();
        publish.insert("rust".to_string(), "cargo publish".to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(workspace.get_publish_command(&config), "cargo publish");
    }

    #[test]
    fn test_get_publish_command_dart() {
        let workspace = MockWorkspace::new(Some("test"), "/project/pubspec.yaml", "pubspec.yaml")
            .with_language(Language::Dart);
        let mut publish = HashMap::new();
        publish.insert("dart".to_string(), "dart pub publish".to_string());
        let config = Config {
            publish,
            ..Default::default()
        };

        assert_eq!(workspace.get_publish_command(&config), "dart pub publish");
    }

    #[test]
    fn test_get_publish_command_default() {
        let workspace = MockWorkspace::new(Some("test"), "/project/package.json", "package.json");
        let config = Config::default();

        assert_eq!(workspace.get_publish_command(&config), "echo publish");
    }

    #[tokio::test]
    async fn test_publish_success() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let workspace = MockWorkspace::new(Some("test"), path.to_str().unwrap(), "package.json");
        let config = Config::default();

        // This will run "echo publish" which should succeed
        let result = workspace.publish(&config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_publish_failure() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("package.json");
        let workspace = MockWorkspace::new(Some("test"), path.to_str().unwrap(), "package.json");
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

        let result = workspace.publish(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_update_workspace_dependencies_default() {
        let workspace = MockWorkspace::new(Some("test"), "/project/package.json", "package.json");
        let packages: Vec<&dyn Package> = vec![];

        let result = workspace.update_workspace_dependencies(&packages).await;
        assert!(result.is_ok());
    }
}
