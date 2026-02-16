use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::DartPackage, workspace::DartWorkspace};

#[derive(Debug)]
pub struct DartProjectFinder {
    projects: HashMap<PathBuf, Project>,
    project_files: Vec<&'static str>,
}

impl Default for DartProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl DartProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec!["pubspec.yaml"],
        }
    }
}

#[async_trait]
impl ProjectFinder for DartProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }
    fn projects_mut(&mut self) -> Vec<&mut Project> {
        self.projects.values_mut().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &self.project_files
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        // glob all the pubspec.yaml in the root without .gitignore
        if path.is_file()
            && self.project_files().contains(
                &path
                    .file_name()
                    .context(format!("File name not found - {}", path.display()))?
                    .to_str()
                    .context(format!("File name not found - {}", path.display()))?,
            )
        {
            if self.projects.contains_key(path) {
                return Ok(());
            }
            // read pubspec.yaml
            let pubspec_yaml = read_to_string(path).await?;
            let pubspec: serde_yaml::Value = serde_yaml::from_str(&pubspec_yaml)?;

            // Check if this is a workspace (melos workspace or similar)
            let is_workspace = pubspec.get("workspace").is_some()
                || path
                    .parent()
                    .context("Parent not found")?
                    .join("melos.yaml")
                    .is_file();

            let (path, mut project) = if is_workspace {
                let version = pubspec["version"]
                    .as_str()
                    .map(std::string::ToString::to_string);
                let name = pubspec["name"]
                    .as_str()
                    .map(std::string::ToString::to_string);
                (
                    path.to_path_buf(),
                    Project::Workspace(Box::new(DartWorkspace::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                )
            } else {
                let version = pubspec["version"]
                    .as_str()
                    .map(std::string::ToString::to_string);
                let name = pubspec["name"]
                    .as_str()
                    .map(std::string::ToString::to_string);

                (
                    path.to_path_buf(),
                    Project::Package(Box::new(DartPackage::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                )
            };

            // read dependencies section
            if let Some(dependencies) = pubspec.get("dependencies").and_then(|d| d.as_mapping()) {
                for (dep_name, _) in dependencies {
                    if let Some(dep_str) = dep_name.as_str() {
                        project.add_dependency(dep_str);
                    }
                }
            }
            self.projects.insert(path, project);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_new() {
        let finder = DartProjectFinder::new();
        assert_eq!(finder.project_files(), &["pubspec.yaml"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_default() {
        let finder = DartProjectFinder::default();
        assert_eq!(finder.project_files(), &["pubspec.yaml"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_visit_package() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
"#,
        )
        .unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);
        match finder.projects()[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("test_package"));
                assert_eq!(pkg.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_workspace_with_workspace_field() {
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

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);
        match finder.projects()[0] {
            Project::Workspace(ws) => {
                assert_eq!(ws.name(), Some("test_workspace"));
                assert_eq!(ws.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_workspace_with_melos_yaml() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        let melos_path = temp_dir.path().join("melos.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
version: 1.0.0
"#,
        )
        .unwrap();
        fs::write(&melos_path, r#"name: test_workspace"#).unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);
        match finder.projects()[0] {
            Project::Workspace(ws) => {
                assert_eq!(ws.name(), Some("test_workspace"));
                assert_eq!(ws.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_workspace_without_version() {
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

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);
        match finder.projects()[0] {
            Project::Workspace(ws) => {
                assert_eq!(ws.name(), Some("test_workspace"));
                assert_eq!(ws.version(), None);
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_non_pubspec_file() {
        let temp_dir = TempDir::new().unwrap();
        let other_file = temp_dir.path().join("other.yaml");
        fs::write(&other_file, r#"some: content"#).unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&other_file, &PathBuf::from("other.yaml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_directory() {
        let temp_dir = TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("some_dir");
        fs::create_dir_all(&dir_path).unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&dir_path, &PathBuf::from("some_dir"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
"#,
        )
        .unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_multiple_packages() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec1 = temp_dir.path().join("package1").join("pubspec.yaml");
        let pubspec2 = temp_dir.path().join("package2").join("pubspec.yaml");
        fs::create_dir_all(pubspec1.parent().unwrap()).unwrap();
        fs::create_dir_all(pubspec2.parent().unwrap()).unwrap();
        fs::write(
            &pubspec1,
            r#"name: package1
version: 1.0.0
"#,
        )
        .unwrap();
        fs::write(
            &pubspec2,
            r#"name: package2
version: 2.0.0
"#,
        )
        .unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec1, &PathBuf::from("package1/pubspec.yaml"))
            .await
            .unwrap();
        finder
            .visit(&pubspec2, &PathBuf::from("package2/pubspec.yaml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 2);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_projects_mut() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
"#,
        )
        .unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();

        let mut projects = finder.projects_mut();
        assert_eq!(projects.len(), 1);
        match &mut projects[0] {
            Project::Package(pkg) => {
                assert!(!pkg.is_changed());
                pkg.set_changed(true);
                assert!(pkg.is_changed());
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_with_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
dependencies:
  http: ^1.0.0
  core:
    path: ../core
  utils:
    path: ../utils
"#,
        )
        .unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("test_package"));
                let deps = pkg.dependencies();
                assert_eq!(deps.len(), 3);
                assert!(deps.contains("http"));
                assert!(deps.contains("core"));
                assert!(deps.contains("utils"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }
}
