use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::next_version;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::detect_package_manager_recursive;

#[derive(Debug)]
pub struct NodePackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl NodePackage {
    #[must_use]
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
            dependencies: HashSet::new(),
        }
    }
}

#[async_trait]
impl Package for NodePackage {
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

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let current_version = self.version.as_deref().unwrap_or("0.0.0");
        let new_version = next_version(current_version, update_type)?;
        crate::write_package_json_version(&self.path, &new_version).await?;
        self.version = Some(new_version);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Node
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    fn default_publish_command(&self) -> String {
        detect_package_manager_recursive(&self.path)
            .publish_command()
            .to_string()
    }

    fn default_dry_run_publish_command(&self) -> Option<String> {
        Some(
            detect_package_manager_recursive(&self.path)
                .dry_run_publish_command()
                .to_string(),
        )
    }

    fn publish_path_dirs(&self) -> Vec<PathBuf> {
        self.path
            .parent()
            .map(crate::node_modules_bin_dirs)
            .unwrap_or_default()
    }

    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }

    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::UpdateType;
    use std::fs;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    #[tokio::test]
    async fn test_node_package_new() {
        let package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
        );

        assert_eq!(package.name(), Some("test-package"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), PathBuf::from("/test/package.json"));
        assert_eq!(package.relative_path(), PathBuf::from("test/package.json"));
        assert_eq!(package.language(), Language::Node);
        assert!(!package.is_changed());
        assert_eq!(package.default_publish_command(), "npm publish");
        assert_eq!(
            package.default_dry_run_publish_command().as_deref(),
            Some("npm publish --dry-run")
        );
    }

    #[tokio::test]
    async fn test_node_package_set_changed() {
        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
        );

        assert!(!package.is_changed());
        package.set_changed(true);
        assert!(package.is_changed());
        package.set_changed(false);
        assert!(!package.is_changed());
    }

    #[tokio::test]
    async fn test_node_package_update_version_patch() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package",
  "version": "1.0.0"
}
"#,
        )
        .unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.0.1""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_package_update_version_minor() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package",
  "version": "1.0.0"
}
"#,
        )
        .unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.1.0""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_package_update_version_major() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package",
  "version": "1.0.0"
}
"#,
        )
        .unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Major).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "2.0.0""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_package_update_version_preserves_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package",
  "version": "1.2.3",
  "description": "A test package",
  "dependencies": {
    "express": "^4.18.0"
  }
}
"#,
        )
        .unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.2.3".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.2.4""#));
        assert!(content.contains(r#""name": "test-package""#));
        assert!(content.contains(r#""description": "A test package""#));
        assert!(content.contains(r#""dependencies""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_package_update_version_preserves_newline() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{"name":"test-package","version":"1.0.0"}
"#,
        )
        .unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.ends_with('\n'));
        assert!(content.contains(r#""version": "1.0.1""#));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_set_name() {
        let mut package = NodePackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("package.json"),
        );
        assert_eq!(package.name(), None);
        package.set_name("my-project".to_string());
        assert_eq!(package.name(), Some("my-project"));
    }

    #[test]
    fn test_publish_path_dirs_includes_node_modules_bin() {
        let temp_dir = TempDir::new().unwrap();
        let bin = temp_dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin).unwrap();
        let package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            temp_dir.path().join("package.json"),
            PathBuf::from("package.json"),
        );
        // Wiring check: the Node override surfaces the local node_modules/.bin
        // so lifecycle hooks (husky) resolve during publish / dry-run.
        assert!(package.publish_path_dirs().contains(&bin));
    }
}
