use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::PythonPackage, workspace::PythonWorkspace};

#[derive(Debug)]
pub struct PythonProjectFinder {
    projects: HashMap<PathBuf, Project>,
    project_files: Vec<&'static str>,
}

impl Default for PythonProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl PythonProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec!["pyproject.toml"],
        }
    }
}

#[async_trait]
impl ProjectFinder for PythonProjectFinder {
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
        if path.is_file()
            && self.project_files().contains(
                &path
                    .file_name()
                    .with_context(|| format!("File name not found - {}", path.display()))?
                    .to_str()
                    .with_context(|| format!("File name not found - {}", path.display()))?,
            )
        {
            if self.projects.contains_key(path) {
                return Ok(());
            }
            // read pyproject.toml
            let pyproject_toml = read_to_string(path).await?;
            let pyproject_toml: toml_edit::DocumentMut = pyproject_toml.parse()?;
            let project = pyproject_toml
                .get("project")
                .with_context(|| format!("Project not found - {}", path.display()))?;

            // Both branches use the same name/version and the same path;
            // hoist so each branch collapses to a single constructor call.
            let version = project
                .get("version")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string);
            let name = project
                .get("name")
                .and_then(|v| v.as_str())
                .map(std::string::ToString::to_string);
            let path_buf = path.to_path_buf();
            let relative_path_buf = relative_path.to_path_buf();

            // if workspace
            let mut project = if pyproject_toml
                .get("tool")
                .and_then(|t| t.get("uv").and_then(|u| u.get("workspace")))
                .is_some()
            {
                Project::Workspace(Box::new(PythonWorkspace::new(
                    name,
                    version,
                    path_buf.clone(),
                    relative_path_buf,
                )))
            } else {
                Project::Package(Box::new(PythonPackage::new(
                    name,
                    version,
                    path_buf.clone(),
                    relative_path_buf,
                )))
            };

            // read tool.uv.sources section
            //
            // `[tool.uv.sources]` is a TOML **table** keyed by dependency name
            // (e.g. `pkg-a = { path = "../pkg-a" }`), not an array of strings.
            // Iterate it as a table so Python packages actually register their
            // workspace deps for topological publish ordering.
            if let Some(sources) = pyproject_toml
                .get("tool")
                .and_then(|t| t.get("uv"))
                .and_then(|u| u.get("sources"))
                .and_then(toml_edit::Item::as_table_like)
            {
                for (dep_name, _) in sources.iter() {
                    project.add_dependency(dep_name);
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
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_python_project_finder_new() {
        let finder = PythonProjectFinder::new();
        assert_eq!(finder.project_files(), &["pyproject.toml"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[test]
    fn test_python_project_finder_default() {
        let finder = PythonProjectFinder::default();
        assert_eq!(finder.project_files(), &["pyproject.toml"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_python_project_finder_visit_package() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[project]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
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
    async fn test_python_project_finder_visit_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]

[project]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
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
    async fn test_python_project_finder_visit_workspace_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]

[project]
name = "test-workspace"
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
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
    async fn test_python_project_finder_visit_non_pyproject_file() {
        let temp_dir = TempDir::new().unwrap();
        let other_file = temp_dir.path().join("other.txt");
        fs::write(&other_file, "some content").unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&other_file, &PathBuf::from("other.txt"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_project_finder_visit_directory() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[project]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        // Pass directory instead of file
        finder
            .visit(temp_dir.path(), &PathBuf::from("."))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_project_finder_visit_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[project]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        // Visit again - should not add duplicate
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_project_finder_visit_multiple_packages() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml1 = temp_dir.path().join("package1").join("pyproject.toml");
        fs::create_dir_all(pyproject_toml1.parent().unwrap()).unwrap();
        fs::write(
            &pyproject_toml1,
            r#"[project]
name = "package1"
version = "1.0.0"
"#,
        )
        .unwrap();

        let pyproject_toml2 = temp_dir.path().join("package2").join("pyproject.toml");
        fs::create_dir_all(pyproject_toml2.parent().unwrap()).unwrap();
        fs::write(
            &pyproject_toml2,
            r#"[project]
name = "package2"
version = "2.0.0"
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml1, &PathBuf::from("package1/pyproject.toml"))
            .await
            .unwrap();
        finder
            .visit(&pyproject_toml2, &PathBuf::from("package2/pyproject.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_project_finder_projects_mut() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[project]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await
            .unwrap();

        let mut_projects = finder.projects_mut();
        assert_eq!(mut_projects.len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_project_finder_visit_package_without_project_section() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[build-system]
requires = ["setuptools"]
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        let result = finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await;

        assert!(result.is_err());
        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_project_finder_visit_registers_uv_sources_dependencies() {
        // Regression: `[tool.uv.sources]` is a TOML **table** keyed by
        // dependency name (`pkg-a = { path = "..." }`), not an array of
        // strings. The finder must iterate it as a table so Python
        // workspaces feed real deps into `sort_by_dependencies` for
        // topological publish order.
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]

[tool.uv.sources]
pkg-a = { path = "packages/pkg-a" }
pkg-b = { workspace = true }

[project]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Workspace(ws) => {
                let deps = ws.dependencies();
                assert_eq!(
                    deps.len(),
                    2,
                    "expected both tool.uv.sources entries, got {deps:?}"
                );
                assert!(deps.contains("pkg-a"), "missing pkg-a in {deps:?}");
                assert!(deps.contains("pkg-b"), "missing pkg-b in {deps:?}");
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }
}
