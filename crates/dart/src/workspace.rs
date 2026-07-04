use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use changepacks_utils::next_version;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::write_pubspec_version;

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
        let current_version = self.version.as_deref().unwrap_or("0.0.0");
        let next_version = next_version(current_version, update_type)?;

        let existing_version = self.version.is_some();
        write_pubspec_version(&self.path, &next_version, existing_version).await?;
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
        assert_eq!(
            workspace.default_dry_run_publish_command().as_deref(),
            Some("dart pub publish --dry-run")
        );

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

    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_update_version_with_existing_version(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
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

        workspace.update_version(update_type).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains(&format!("version: {expected}")));

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

    #[test]
    fn test_set_name() {
        let mut workspace = DartWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("pubspec.yaml"),
        );
        assert_eq!(workspace.name(), None);
        workspace.set_name("my-project".to_string());
        assert_eq!(workspace.name(), Some("my-project"));
    }
}
