use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{package::NodePackage, read_and_parse_package_json, workspace::NodeWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["package.json"];

/// Look up a field in the `package.json` manifest as an owned string, mirroring the
/// `doc.get(field).and_then(|v| v.as_str()).map(ToString::to_string)` chain that
/// used to be open-coded twice inside `visit` (once for `version`, once for `name`).
/// Extracted so a future manifest shape change only needs to be adapted in one place —
/// matches the `project_str` idiom in [`crate::finder`]'s sibling Python finder
/// ([`crates/python/src/finder.rs`](../../../python/src/finder.rs)).
fn package_json_str(doc: &serde_json::Value, field: &str) -> Option<String> {
    doc.get(field)
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
}

#[derive(Debug, Default)]
pub struct NodeProjectFinder {
    projects: HashMap<PathBuf, Project>,
}

impl NodeProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn add_workspace_dependencies(project: &mut Project, package_json: &serde_json::Value) {
    for section in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        let Some(deps) = package_json.get(section).and_then(|deps| deps.as_object()) else {
            continue;
        };
        for (dep_name, value) in deps {
            if value.as_str().is_some_and(|version| {
                version.starts_with("workspace:") || version.starts_with("file:")
            }) {
                project.add_dependency(dep_name);
            }
        }
    }
}

#[async_trait]
impl ProjectFinder for NodeProjectFinder {
    // `projects()` / `projects_mut()` share their byte-identical body with
    // the Python and Dart finders (all three use a
    // `HashMap<PathBuf, Project>` backing store). Consolidated via the
    // `impl_projects_hashmap_accessors!()` macro in `changepacks-core` so
    // future accessor tweaks land in one place — expansion is byte-
    // identical to the previous hand-rolled bodies.
    changepacks_core::impl_projects_hashmap_accessors!();

    fn project_files(&self) -> &[&str] {
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        // Parse this manifest if it is a recognized project file not already visited.
        if !self.matches_project_file(path).await {
            return Ok(());
        }
        if self.projects.contains_key(path) {
            return Ok(());
        }
        // read package.json
        let (_package_json_raw, package_json) = read_and_parse_package_json(path).await?;
        // Both branches use the same name/version and the same path;
        // hoist so each branch collapses to a single constructor call.
        let version = package_json_str(&package_json, "version");
        let name = package_json_str(&package_json, "name");
        // Rename `path_buf` → `path_key` to align with the Java, CSharp,
        // and Python finders' local naming convention: the
        // value is used once as the `HashMap` insert key (the "key"
        // role), while the branch constructors take their own owned
        // `PathBuf` via `.clone()`. Pure rename — clone count is
        // unchanged.
        let path_key = path.to_path_buf();
        // Rename `relative_path_buf` → `relative_path_key` to match the
        // Dart, Java, and CSharp finders' local naming convention
        // (matches the docstring at `crates/dart/src/finder.rs`
        // claiming "Node, Python, CSharp, Java, and Rust" all use
        // this name). Pure rename — behavior unchanged.
        let relative_path_key = relative_path.to_path_buf();
        // Workspace detection is short-circuited: a `workspaces` field in
        // `package.json` (npm / yarn / bun monorepos — the common case)
        // is enough on its own, so only fall back to a `pnpm-workspace.yaml`
        // stat when that field is absent. Shared with the Dart finder via
        // `changepacks_utils::is_workspace_by_sibling` — the one source of
        // truth for the "declared field OR fixed sibling file" policy
        // (stat error → not-a-workspace; all file ops via `tokio::fs`).
        let is_workspace = changepacks_utils::is_workspace_by_sibling(
            package_json.get("workspaces").is_some(),
            path,
            "pnpm-workspace.yaml",
        )
        .await?;
        let mut project = if is_workspace {
            Project::Workspace(Box::new(NodeWorkspace::new(
                name,
                version,
                path_key.clone(),
                relative_path_key,
            )))
        } else {
            Project::Package(Box::new(NodePackage::new(
                name,
                version,
                path_key.clone(),
                relative_path_key,
            )))
        };

        add_workspace_dependencies(&mut project, &package_json);

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
    "cli": "workspace:~",
    "api": "workspace:^1.2.3",
    "external": "^1.0.0"
  },
  "devDependencies": {
    "test-utils": "workspace:*"
  },
  "peerDependencies": {
    "plugin-api": "workspace:*"
  },
  "peerDependenciesMeta": {
    "plugin-api": {
      "optional": true
    }
  },
  "optionalDependencies": {
    "native-addon": "workspace:*",
    "native-external": "^2.0.0"
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
        assert_eq!(deps.len(), 7);
        assert!(deps.contains("core"));
        assert!(deps.contains("utils"));
        assert!(deps.contains("cli"));
        assert!(deps.contains("api"));
        assert!(deps.contains("test-utils"));
        assert!(deps.contains("plugin-api"));
        assert!(deps.contains("native-addon"));
        assert!(!deps.contains("external"));
        assert!(!deps.contains("native-external"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_package_with_file_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package",
  "version": "1.0.0",
  "dependencies": {
    "foo": "file:../foo",
    "bar": "^1.0.0"
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

        let deps = projects.first().unwrap().dependencies();
        assert_eq!(deps.len(), 1);
        assert!(deps.contains("foo"));
        assert!(!deps.contains("bar"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_project_finder_visit_malformed_package_json() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(&package_json, r#"{ "name": "test", invalid json }"#).unwrap();

        let mut finder = NodeProjectFinder::new();
        let result = finder
            .visit(&package_json, &PathBuf::from("package.json"))
            .await;

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Failed to parse package.json"));
        assert!(error_msg.contains(package_json.to_string_lossy().as_ref()));

        temp_dir.close().unwrap();
    }
}
