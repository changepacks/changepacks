use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{package::PythonPackage, read_and_parse_pyproject_toml, workspace::PythonWorkspace};

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
/// to be adapted in one place — mirrors `package_str` /
/// `workspace_package_str` in the `changepacks-rust` finder.
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
        Self::default()
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
        // Parse this manifest if it is a recognized project file not already
        // visited. Both guards live in `ProjectFinder::should_visit_manifest`
        // (name/stat gate first, already-discovered map probe second) so the
        // prelude is written once for every file-name-based finder.
        if !self.should_visit_manifest(path).await? {
            return Ok(());
        }
        // read and parse pyproject.toml
        let (_raw, pyproject_toml) = read_and_parse_pyproject_toml(path).await?;
        // `[project]` is OPTIONAL: uv workspace-only roots (the docs'
        // canonical example) declare just `[tool.uv.workspace]` at the
        // repo root and no `[project]` table. Match the tolerant
        // extraction that `PythonWorkspace::update_version` already
        // uses (see `write_pyproject_version` in `crates/python/src/lib.rs`).
        // Both name and version fall through to `None` when the table
        // is missing, exactly like the constructor arguments accept.
        let project_table = pyproject_toml.get("project");

        // Both branches use the same name/version and the same path;
        // hoist so each branch collapses to a single constructor call.
        let version = project_str(project_table, "version");
        let name = project_str(project_table, "name");
        let publishable_by_default = name.is_some();
        let path_key = path.to_path_buf();
        let relative_path_key = relative_path.to_path_buf();

        // Hoist the `[tool.uv]` lookup ONCE: both the workspace guard
        // below and the `[tool.uv.sources]` walk further down previously
        // walked the identical `pyproject_toml.get("tool").and_then(|t|
        // t.get("uv"))` chain independently. `Option<&Item>` is `Copy`,
        // so caching the intermediate binding lets both call sites reuse
        // it — saves one HashMap-style lookup per Python-project visit
        // on every `check` / `update` / `publish` invocation. Behavior
        // is byte-identical: both branches short-circuit on the same
        // `None` positions.
        let uv_table = pyproject_toml.get("tool").and_then(|t| t.get("uv"));

        // The absent/valid/invalid triage is
        // `changepacks_utils::ensure_declared_shape`, shared verbatim with the
        // Node and Dart finders; only the shape predicate stays here because it
        // is the one part that speaks `toml_edit::Item`.
        let has_workspace_declaration = changepacks_utils::ensure_declared_shape(
            uv_table
                .and_then(|u| u.get("workspace"))
                .map(|workspace| workspace.as_table_like().is_some()),
            path,
            "[tool.uv].workspace",
            "a table or inline table",
        )?;

        let mut project = changepacks_core::discovered_project!(
            has_workspace_declaration,
            PythonWorkspace::new_discovered,
            PythonPackage::new_discovered,
            name,
            version,
            path_key.clone(),
            relative_path_key,
            publishable_by_default,
        );

        // read tool.uv.sources section
        //
        // `[tool.uv.sources]` is a TOML **table** keyed by dependency name
        // (e.g. `pkg-a = { path = "../pkg-a" }`), not an array of strings.
        // Iterate it as a table so Python packages actually register their
        // workspace deps for topological publish ordering.
        if let Some(sources) = uv_table
            .and_then(|u| u.get("sources"))
            .and_then(toml_edit::Item::as_table_like)
        {
            for (dep_name, source) in sources.iter() {
                let is_local_source = source.as_table_like().is_some_and(|source| {
                    source.contains_key("path")
                        || source.get("workspace").and_then(toml_edit::Item::as_bool) == Some(true)
                });
                if is_local_source {
                    project.add_dependency(dep_name);
                }
            }
        }

        self.projects.insert(path_key, project);
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
        let pkg = projects[0].expect_package();
        assert_eq!(pkg.name(), Some("test-package"));
        assert_eq!(pkg.version(), Some("1.0.0"));
        assert!(pkg.is_publishable_by_default());

        temp_dir.close().unwrap();
    }

    #[rstest]
    #[case(
        r#"[tool.uv.workspace]
members = ["packages/*"]
"#
    )]
    #[case("[tool.uv.workspace]\n")]
    #[case(
        r#"[tool.uv]
workspace = { members = ["packages/*"] }
"#
    )]
    #[case("[tool.uv]\nworkspace = {}\n")]
    #[tokio::test]
    async fn test_python_project_finder_visit_workspace_with_table_like_declaration(
        #[case] workspace: &str,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            format!(
                r#"{workspace}
[project]
name = "test-workspace"
version = "1.0.0"
"#
            ),
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        let ws = projects[0].expect_workspace();
        assert_eq!(ws.name(), Some("test-workspace"));
        assert_eq!(ws.version(), Some("1.0.0"));
        assert!(ws.is_publishable_by_default());

        temp_dir.close().unwrap();
    }

    #[rstest]
    #[case("true")]
    #[case(r#""packages/*""#)]
    #[case("[]")]
    #[tokio::test]
    async fn test_python_project_finder_rejects_invalid_uv_workspace_declaration(
        #[case] workspace: &str,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            format!(
                r#"[tool.uv]
workspace = {workspace}

[project]
name = "test-package"
version = "1.0.0"
"#
            ),
        )
        .unwrap();

        let mut finder = PythonProjectFinder::new();
        let result = finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await;

        let error_msg = result
            .expect_err("invalid uv workspace declaration should fail")
            .to_string();
        assert!(
            error_msg.contains("Invalid `[tool.uv].workspace` declaration"),
            "error message should explain the invalid declaration, got: {error_msg}"
        );
        assert!(
            error_msg.contains(pyproject_toml.to_string_lossy().as_ref()),
            "error message should contain the manifest path, got: {error_msg}"
        );
        assert!(finder.projects().is_empty());

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
        let ws = projects[0].expect_workspace();
        assert_eq!(ws.name(), Some("test-workspace"));
        assert_eq!(ws.version(), None);

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
        // failing hard — `write_pyproject_version` already handles the
        // missing-section case downstream (it creates `[project]` on
        // demand), so the extraction here must be lenient too.
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

        let mut projects = finder.projects_mut();
        assert_eq!(projects.len(), 1);
        let project = projects.pop().unwrap();
        assert!(matches!(&project, Project::Package(_)));
        assert_eq!(project.name(), None);
        assert_eq!(project.version(), None);
        assert!(!project.is_publishable_by_default());

        project.set_name("repository-name".to_string());

        assert_eq!(project.name(), Some("repository-name"));
        assert!(!project.is_publishable_by_default());

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
        let ws = projects[0].expect_workspace();
        assert_eq!(ws.name(), None);
        assert_eq!(ws.version(), None);
        assert!(!ws.is_publishable_by_default());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_workspace_only_root_fallback_name_does_not_enable_default_publish() {
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

        let mut projects = finder.projects_mut();
        assert_eq!(projects.len(), 1);
        let project = projects.pop().unwrap();
        assert!(!project.is_publishable_by_default());

        project.set_name("repository-name".to_string());

        assert_eq!(project.name(), Some("repository-name"));
        assert!(!project.is_publishable_by_default());

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
		pkg-a = { path = "packages/pkg-a", editable = true }
		pkg-b = { workspace = true }
		pkg-c = { git = "https://example.com/pkg-c.git", editable = true }
		pkg-d = { url = "https://example.com/pkg-d.tar.gz" }
		pkg-e = { workspace = false }

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
        let deps = projects[0].expect_workspace().dependencies();
        assert_eq!(
            deps.len(),
            2,
            "expected only local tool.uv.sources entries, got {deps:?}"
        );
        assert!(deps.contains("pkg-a"), "missing pkg-a in {deps:?}");
        assert!(deps.contains("pkg-b"), "missing pkg-b in {deps:?}");
        assert!(!deps.contains("pkg-c"), "unexpected pkg-c in {deps:?}");
        assert!(!deps.contains("pkg-d"), "unexpected pkg-d in {deps:?}");
        assert!(!deps.contains("pkg-e"), "unexpected pkg-e in {deps:?}");

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_project_finder_visit_malformed_manifest() {
        // Regression: malformed pyproject.toml must fail with path-aware
        // error context. The error message must include both the manifest
        // path and "Failed to parse pyproject.toml".
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(&pyproject_toml, "invalid toml [[[").unwrap();

        let mut finder = PythonProjectFinder::new();
        let result = finder
            .visit(&pyproject_toml, &PathBuf::from("pyproject.toml"))
            .await;

        assert!(result.is_err(), "Expected error for malformed manifest");
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Failed to parse pyproject.toml"),
            "Error message missing 'Failed to parse pyproject.toml': {error_msg}"
        );
        assert!(
            error_msg.contains(pyproject_toml.to_string_lossy().as_ref()),
            "Error message missing path: {error_msg}"
        );

        temp_dir.close().unwrap();
    }
}
