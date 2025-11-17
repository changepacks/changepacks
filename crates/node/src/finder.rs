use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::NodePackage, workspace::NodeWorkspace};

#[derive(Debug)]
pub struct NodeProjectFinder {
    projects: HashMap<PathBuf, Project>,
    project_files: Vec<&'static str>,
}

impl Default for NodeProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec!["package.json"],
        }
    }
}

#[async_trait]
impl ProjectFinder for NodeProjectFinder {
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
        // glob all the package.json in the root without .gitignore
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
            // read package.json
            let package_json = read_to_string(path).await?;
            let package_json: serde_json::Value = serde_json::from_str(&package_json)?;
            // if workspaces
            if package_json.get("workspaces").is_some()
                || path
                    .parent()
                    .context(format!("Parent not found - {}", path.display()))?
                    .join("pnpm-workspace.yaml")
                    .is_file()
            {
                let version = package_json["version"].as_str().map(|v| v.to_string());
                let name = package_json["name"].as_str().map(|v| v.to_string());
                self.projects.insert(
                    path.to_path_buf(),
                    Project::Workspace(Box::new(NodeWorkspace::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                );
            } else {
                let version = package_json["version"]
                    .as_str()
                    .context(format!("Version not found - {}", path.display()))?
                    .to_string();
                let name = package_json["name"]
                    .as_str()
                    .context(format!("Name not found - {}", path.display()))?
                    .to_string();

                self.projects.insert(
                    path.to_path_buf(),
                    Project::Package(Box::new(NodePackage::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::Project;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_node_project_finder_new() {
        let finder = NodeProjectFinder::new();
        assert_eq!(finder.project_files(), &["package.json"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[test]
    fn test_node_project_finder_default() {
        let finder = NodeProjectFinder::default();
        assert_eq!(finder.project_files(), &["package.json"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_package() {
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

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), "test-package");
                assert_eq!(pkg.version(), "1.0.0");
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_workspace_with_workspaces() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-workspace",
  "version": "1.0.0",
  "workspaces": ["packages/*"]
}
"#,
        )
        .unwrap();

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Workspace(ws) => {
                assert_eq!(ws.name(), Some("test-workspace"));
                assert_eq!(ws.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_workspace_with_pnpm_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-workspace",
  "version": "1.0.0"
}
"#,
        )
        .unwrap();

        // Create pnpm-workspace.yaml
        let pnpm_workspace = temp_dir.path().join("pnpm-workspace.yaml");
        fs::write(&pnpm_workspace, "packages:\n  - 'packages/*'\n").unwrap();

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Workspace(ws) => {
                assert_eq!(ws.name(), Some("test-workspace"));
                assert_eq!(ws.version(), Some("1.0.0"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_workspace_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-workspace",
  "workspaces": ["packages/*"]
}
"#,
        )
        .unwrap();

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Workspace(ws) => {
                assert_eq!(ws.name(), Some("test-workspace"));
                assert_eq!(ws.version(), None);
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_non_package_file() {
        let temp_dir = TempDir::new().unwrap();
        let other_file = temp_dir.path().join("other.txt");
        fs::write(&other_file, "some content").unwrap();

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&other_file, &PathBuf::from("other.txt"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_directory() {
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

        let mut finder = NodeProjectFinder::new();
        // Pass directory instead of file
        finder
            .visit(temp_dir.path(), &PathBuf::from("."))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_duplicate() {
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

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        // Visit again - should not add duplicate
        finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_multiple_packages() {
        let temp_dir = TempDir::new().unwrap();
        let package_json1 = temp_dir.path().join("package1").join("package.json");
        fs::create_dir_all(package_json1.parent().unwrap()).unwrap();
        fs::write(
            &package_json1,
            r#"{
  "name": "package1",
  "version": "1.0.0"
}
"#,
        )
        .unwrap();

        let package_json2 = temp_dir.path().join("package2").join("package.json");
        fs::create_dir_all(package_json2.parent().unwrap()).unwrap();
        fs::write(
            &package_json2,
            r#"{
  "name": "package2",
  "version": "2.0.0"
}
"#,
        )
        .unwrap();

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&package_json1, &PathBuf::from("package1/package.json"))
            .await
            .unwrap();
        finder
            .visit(&package_json2, &PathBuf::from("package2/package.json"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_projects_mut() {
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

        let mut finder = NodeProjectFinder::new();
        finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await
            .unwrap();

        let mut_projects = finder.projects_mut();
        assert_eq!(mut_projects.len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_package_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package"
}
"#,
        )
        .unwrap();

        let mut finder = NodeProjectFinder::new();
        let result = finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await;

        assert!(result.is_err());
        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_package_without_name() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "version": "1.0.0"
}
"#,
        )
        .unwrap();

        let mut finder = NodeProjectFinder::new();
        let result = finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await;

        assert!(result.is_err());
        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }
}
