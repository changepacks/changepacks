use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::PythonPackage, workspace::PythonWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["pyproject.toml"];

/// Look up `[project].<field>` as an owned string, mirroring the
/// `project.and_then(|p| p.get(field)).and_then(|v| v.as_str()).map(String::from)`
/// chain that used to be open-coded twice inside `visit` (once for
/// `version`, once for `name`). Extracted so a future manifest shape
/// change (e.g. inline-table `name = { workspace = true }`) only needs
/// to be adapted in one place — matches the `package_str` /
/// `workspace_package_str` idiom in [`crate::finder`]'s sibling Rust
/// finder ([`crates/rust/src/finder.rs`](../../../rust/src/finder.rs)).
fn project_str(project: Option<&toml_edit::Item>, field: &str) -> Option<String> {
    project
        .and_then(|p| p.get(field))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
}

#[derive(Debug, Default)]
pub struct PythonProjectFinder {
    projects: HashMap<PathBuf, Project>,
}

impl PythonProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
        }
    }
}

#[async_trait]
impl ProjectFinder for PythonProjectFinder {
    // `projects()` / `projects_mut()` share their byte-identical body with
    // the Node and Dart finders (all three use a
    // `HashMap<PathBuf, Project>` backing store). Consolidated via the
    // `impl_projects_hashmap_accessors!()` macro in `changepacks-core` so
    // future accessor tweaks land in one place — expansion is byte-
    // identical to the previous hand-rolled bodies.
    changepacks_core::impl_projects_hashmap_accessors!();

    fn project_files(&self) -> &[&str] {
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        if self.matches_project_file(path).await? {
            if self.projects.contains_key(path) {
                return Ok(());
            }
            // read pyproject.toml
            let pyproject_toml = read_to_string(path).await?;
            let pyproject_toml: toml_edit::DocumentMut = pyproject_toml.parse()?;
            // `[project]` is OPTIONAL: uv workspace-only roots (the docs'
            // canonical example) declare just `[tool.uv.workspace]` at the
            // repo root and no `[project]` table. Match the tolerant
            // extraction that `PythonWorkspace::update_version` already
            // uses (see `ensure_project_table` in `crates/python/src/lib.rs`).
            // Both name and version fall through to `None` when the table
            // is missing, exactly like the constructor arguments accept.
            let project = pyproject_toml.get("project");

            // Both branches use the same name/version and the same path;
            // hoist so each branch collapses to a single constructor call.
            let version = project_str(project, "version");
            let name = project_str(project, "name");
            // Rename `path_buf` → `path_key` to align with the Java and
            // CSharp finders' local naming convention: the value is used
            // once as the `HashMap` insert key (the "key" role), while
            // the branch constructors take their own owned `PathBuf` via
            // `.clone()`. Pure rename — clone count is unchanged.
            let path_key = path.to_path_buf();
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
                    path_key.clone(),
                    relative_path_buf,
                )))
            } else {
                Project::Package(Box::new(PythonPackage::new(
                    name,
                    version,
                    path_key.clone(),
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

            self.projects.insert(path_key, project);
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

    // Both `PythonProjectFinder::new()` and `PythonProjectFinder::default()`
    // must yield the same empty, `pyproject.toml`-scoped finder.
    #[rstest]
    #[case(PythonProjectFinder::new())]
    #[case(PythonProjectFinder::default())]
    fn test_python_project_finder_construction(#[case] finder: PythonProjectFinder) {
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
        // Regression: a pyproject.toml with only `[build-system]` (no
        // `[project]`, no `[tool.uv.workspace]`) is a legitimate PEP 517
        // shape used e.g. by build-only backend configs. The finder must
        // register it as a Package with `None` name/version rather than
        // failing hard — `PythonWorkspace::update_version` and
        // `ensure_project_table` already handle the missing-section case
        // downstream, so the extraction here must be lenient too.
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
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), None);
                assert_eq!(pkg.version(), None);
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_project_finder_visit_workspace_without_project_section() {
        // Regression: uv workspace-only roots (the docs' canonical
        // example) declare just `[tool.uv.workspace]` at the repo root
        // with no `[project]` table at all. Members supply their own
        // `[project]` sections. The finder must register the root as a
        // `Project::Workspace` with `None` name/version, mirroring how
        // `PythonWorkspace::update_version` (see
        // `test_python_workspace_update_version_without_project_section`)
        // already handles the missing-section case downstream.
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]
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
                assert_eq!(ws.name(), None);
                assert_eq!(ws.version(), None);
            }
            _ => panic!("Expected Workspace"),
        }

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
