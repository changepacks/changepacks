use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::next_version;

use crate::write_pubspec_version;

#[derive(Debug)]
pub struct DartPackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl DartPackage {
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
impl Package for DartPackage {
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

        write_pubspec_version(&self.path, &new_version, true).await?;
        self.version = Some(new_version);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Dart
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }
    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    fn default_publish_command(&self) -> String {
        crate::PUBLISH_COMMAND.to_string()
    }

    fn default_dry_run_publish_command(&self) -> Option<String> {
        Some(crate::DRY_RUN_PUBLISH_COMMAND.to_string())
    }

    // `dependencies()` / `add_dependency()` share their byte-identical
    // body with every other language crate's `Package` and `Workspace`
    // impl (all use `dependencies: HashSet<String>` as their backing
    // store). Consolidated via the `impl_dependencies_accessors!()`
    // macro in `changepacks-core` so future accessor tweaks land in
    // one place — expansion is byte-identical to the previous
    // hand-rolled bodies.
    changepacks_core::impl_dependencies_accessors!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_new() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
"#,
        )
        .unwrap();

        let package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert_eq!(package.name(), Some("test_package"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), pubspec_path);
        assert_eq!(package.relative_path(), PathBuf::from("pubspec.yaml"));
        assert!(!package.is_changed());
        assert_eq!(package.language(), Language::Dart);
        assert_eq!(package.default_publish_command(), "dart pub publish");
        assert_eq!(
            package.default_dry_run_publish_command().as_deref(),
            Some("dart pub publish --dry-run")
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_set_changed() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
"#,
        )
        .unwrap();

        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert!(!package.is_changed());
        package.set_changed(true);
        assert!(package.is_changed());
        package.set_changed(false);
        assert!(!package.is_changed());

        temp_dir.close().unwrap();
    }

    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_update_version(#[case] update_type: UpdateType, #[case] expected: &str) {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
"#,
        )
        .unwrap();

        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        package.update_version(update_type).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains(&format!("version: {expected}")));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_preserves_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        let original_content = r#"name: test_package
version: 1.0.0
description: A test package
dependencies:
  http: ^1.0.0
"#;
        fs::write(&pubspec_path, original_content).unwrap();

        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 1.0.1"));
        assert!(content.contains("name: test_package"));
        assert!(content.contains("description: A test package"));
        assert!(content.contains("dependencies:"));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_dependencies() {
        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("test/pubspec.yaml"),
        );

        // Initially empty
        assert!(package.dependencies().is_empty());

        // Add dependencies
        package.add_dependency("http");
        package.add_dependency("core");

        let deps = package.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("http"));
        assert!(deps.contains("core"));

        // Adding duplicate should not increase count
        package.add_dependency("http");
        assert_eq!(package.dependencies().len(), 2);
    }

    #[test]
    fn test_set_name() {
        let mut package = DartPackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("pubspec.yaml"),
        );
        assert_eq!(package.name(), None);
        package.set_name("my-project".to_string());
        assert_eq!(package.name(), Some("my-project"));
    }
}
