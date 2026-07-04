use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::NodePackage, workspace::NodeWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["package.json"];

#[derive(Debug, Default)]
pub struct NodeProjectFinder {
    projects: HashMap<PathBuf, Project>,
}

impl NodeProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
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
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        // glob all the package.json in the root without .gitignore
        if self.matches_project_file(path).await? {
            if self.projects.contains_key(path) {
                return Ok(());
            }
            // read package.json
            let package_json = read_to_string(path).await?;
            let package_json: serde_json::Value = serde_json::from_str(&package_json)?;
            // Both branches use the same name/version and the same path;
            // hoist so each branch collapses to a single constructor call.
            let version = package_json["version"]
                .as_str()
                .map(std::string::ToString::to_string);
            let name = package_json["name"]
                .as_str()
                .map(std::string::ToString::to_string);
            let path_buf = path.to_path_buf();
            let relative_path_buf = relative_path.to_path_buf();
            // Workspace detection is short-circuited: a `workspaces` field in
            // `package.json` (npm / yarn / bun monorepos — the common case)
            // is enough on its own, so only fall back to a `pnpm-workspace.yaml`
            // stat when that field is absent. This skips one async filesystem
            // syscall + one `PathBuf` allocation per non-pnpm workspace.
            // AGENTS.md rule: all file ops via `tokio::fs`. `try_exists`
            // treats a stat error (broken symlink, permission denied) as
            // "does not exist", matching the previous sync `is_file()`
            // fallthrough on error.
            let is_workspace = if package_json.get("workspaces").is_some() {
                true
            } else {
                let pnpm_workspace_yaml = path
                    .parent()
                    .with_context(|| format!("Parent not found - {}", path.display()))?
                    .join("pnpm-workspace.yaml");
                tokio::fs::try_exists(&pnpm_workspace_yaml)
                    .await
                    .unwrap_or(false)
            };
            let mut project = if is_workspace {
                Project::Workspace(Box::new(NodeWorkspace::new(
                    name,
                    version,
                    path_buf.clone(),
                    relative_path_buf,
                )))
            } else {
                Project::Package(Box::new(NodePackage::new(
                    name,
                    version,
                    path_buf.clone(),
                    relative_path_buf,
                )))
            };

            if let Some(deps) = package_json.get("dependencies").and_then(|d| d.as_object()) {
                for (dep_name, value) in deps {
                    // Only track workspace:* dependencies (exact version sync)
                    // workspace:^ uses semver ranges so doesn't need forced updates
                    if value.as_str() == Some("workspace:*") {
                        project.add_dependency(dep_name);
                    }
                }
            }

            self.projects.insert(path_buf, project);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::Project;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;

    // Both `NodeProjectFinder::new()` and `NodeProjectFinder::default()` must
    // yield the same empty, `package.json`-scoped finder.
    #[rstest]
    #[case(NodeProjectFinder::new())]
    #[case(NodeProjectFinder::default())]
    fn test_node_project_finder_construction(#[case] finder: NodeProjectFinder) {
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
                assert_eq!(pkg.name(), Some("test-package"));
                assert_eq!(pkg.version(), Some("1.0.0"));
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
    async fn test_node_project_finder_visit_package_with_workspace_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package",
  "version": "1.0.0",
  "dependencies": {
    "core": "workspace:*",
    "utils": "workspace:^",
    "external": "^1.0.0"
  }
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

        let project = projects.first().unwrap();
        let deps = project.dependencies();
        // Only workspace:* dependencies should be tracked
        assert_eq!(deps.len(), 1);
        assert!(deps.contains("core"));
        // workspace:^ and external deps should not be tracked
        assert!(!deps.contains("utils"));
        assert!(!deps.contains("external"));

        temp_dir.close().unwrap();
    }
}
