use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::next_version;
use tokio::fs::{read_to_string, write};

#[derive(Debug)]
pub struct DartPackage {
    name: String,
    version: String,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
}

impl DartPackage {
    pub fn new(name: String, version: String, path: PathBuf, relative_path: PathBuf) -> Self {
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
        }
    }
}

#[async_trait]
impl Package for DartPackage {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(&self.version, update_type)?;

        let pubspec_yaml_raw = read_to_string(&self.path).await?;
        write(
            &self.path,
            format!(
                "{}{}",
                yamlpatch::apply_yaml_patches(
                    &yamlpath::Document::new(&pubspec_yaml_raw).context("Failed to parse YAML")?,
                    &[yamlpatch::Patch {
                        operation: yamlpatch::Op::Replace(serde_yaml::Value::String(
                            next_version.clone()
                        )),
                        route: yamlpath::route!("version"),
                    }],
                )?
                .source()
                .trim_end(),
                if pubspec_yaml_raw.ends_with("\n") {
                    "\n"
                } else {
                    ""
                }
            ),
        )
        .await?;
        self.version = next_version;
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

    fn default_publish_command(&self) -> &'static str {
        "dart pub publish"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            "test_package".to_string(),
            "1.0.0".to_string(),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert_eq!(package.name(), "test_package");
        assert_eq!(package.version(), "1.0.0");
        assert_eq!(package.path(), pubspec_path);
        assert_eq!(package.relative_path(), PathBuf::from("pubspec.yaml"));
        assert_eq!(package.is_changed(), false);
        assert_eq!(package.language(), Language::Dart);
        assert_eq!(package.default_publish_command(), "dart pub publish");

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
            "test_package".to_string(),
            "1.0.0".to_string(),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert_eq!(package.is_changed(), false);
        package.set_changed(true);
        assert_eq!(package.is_changed(), true);
        package.set_changed(false);
        assert_eq!(package.is_changed(), false);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_patch() {
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
            "test_package".to_string(),
            "1.0.0".to_string(),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 1.0.1"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_minor() {
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
            "test_package".to_string(),
            "1.0.0".to_string(),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        package.update_version(UpdateType::Minor).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 1.1.0"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_major() {
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
            "test_package".to_string(),
            "1.0.0".to_string(),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        package.update_version(UpdateType::Major).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 2.0.0"));

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
            "test_package".to_string(),
            "1.0.0".to_string(),
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
}
