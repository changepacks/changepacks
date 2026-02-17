use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use changepacks_utils::next_version;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};

#[derive(Debug)]
pub struct DartWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl DartWorkspace {
    #[must_use]
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self {
            path,
            relative_path,
            name,
            version,
            is_changed: false,
            dependencies: HashSet::new(),
        }
    }
}

#[async_trait]
impl Workspace for DartWorkspace {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(
            self.version.as_ref().unwrap_or(&String::from("0.0.0")),
            update_type,
        )?;

        let pubspec_yaml_raw = read_to_string(&self.path).await?;

        write(
            &self.path,
            format!(
                "{}{}",
                yamlpatch::apply_yaml_patches(
                    &yamlpath::Document::new(&pubspec_yaml_raw).context("Failed to parse YAML")?,
                    &[yamlpatch::Patch {
                        operation: if self.version.is_some() {
                            yamlpatch::Op::Replace(serde_yaml::Value::String(next_version.clone()))
                        } else {
                            yamlpatch::Op::Add {
                                key: "version".to_string(),
                                value: serde_yaml::Value::String(next_version.clone()),
                            }
                        },
                        route: if self.version.is_some() {
                            yamlpath::route!("version")
                        } else {
                            yamlpath::route!()
                        }
                    }],
                )?
                .source()
                .trim_end(),
                if pubspec_yaml_raw.ends_with('\n') {
                    "\n"
                } else {
                    ""
                }
            ),
        )
        .await?;
        self.version = Some(next_version);
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

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    fn default_publish_command(&self) -> String {
        "dart pub publish".to_string()
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
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_new_with_name_and_version() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
version: 1.0.0
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert_eq!(workspace.name(), Some("test_workspace"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), pubspec_path);
        assert_eq!(workspace.relative_path(), PathBuf::from("pubspec.yaml"));
        assert!(!workspace.is_changed());
        assert_eq!(workspace.language(), Language::Dart);
        assert_eq!(workspace.default_publish_command(), "dart pub publish");

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_new_without_name_and_version() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let workspace = DartWorkspace::new(
            None,
            None,
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
        assert_eq!(workspace.path(), pubspec_path);
        assert!(!workspace.is_changed());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_set_changed() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
version: 1.0.0
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert!(!workspace.is_changed());
        workspace.set_changed(true);
        assert!(workspace.is_changed());
        workspace.set_changed(false);
        assert!(!workspace.is_changed());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_with_existing_version() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
version: 1.0.0
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 1.0.1"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            None,
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 0.0.1"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_minor() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
version: 1.0.0
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        workspace.update_version(UpdateType::Minor).await.unwrap();

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
            r#"name: test_workspace
version: 1.0.0
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        workspace.update_version(UpdateType::Major).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 2.0.0"));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_dependencies() {
        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("test/pubspec.yaml"),
        );

        // Initially empty
        assert!(workspace.dependencies().is_empty());

        // Add dependencies
        workspace.add_dependency("http");
        workspace.add_dependency("core");

        let deps = workspace.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("http"));
        assert!(deps.contains("core"));

        // Adding duplicate should not increase count
        workspace.add_dependency("http");
        assert_eq!(workspace.dependencies().len(), 2);
    }
}
