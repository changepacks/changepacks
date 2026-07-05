use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::DartPackage, workspace::DartWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["pubspec.yaml"];

/// Look up `<field>` on a parsed pubspec as an owned string, mirroring the
/// `pubspec.get(field).and_then(|v| v.as_str()).map(String::from)` chain
/// that used to be open-coded twice inside `visit`. Extracted so the
/// manifest-shape assumption ("top-level key, string value") lives in one
/// place — matches the `project_str` / `package_str` sibling helpers
/// already established in `crates/python/src/finder.rs` and
/// `crates/rust/src/finder.rs`, closing the last workspace-wide gap.
fn pubspec_str(pubspec: &yaml_serde::Value, field: &str) -> Option<String> {
    pubspec
        .get(field)
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
}

#[derive(Debug, Default)]
pub struct DartProjectFinder {
    projects: HashMap<PathBuf, Project>,
}

impl DartProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
        }
    }
}

#[async_trait]
impl ProjectFinder for DartProjectFinder {
    // `projects()` / `projects_mut()` share their byte-identical body with
    // the Node and Python finders (all three use a
    // `HashMap<PathBuf, Project>` backing store). Consolidated via the
    // `impl_projects_hashmap_accessors!()` macro in `changepacks-core` so
    // future accessor tweaks land in one place — expansion is byte-
    // identical to the previous hand-rolled bodies.
    changepacks_core::impl_projects_hashmap_accessors!();

    fn project_files(&self) -> &[&str] {
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        // glob all the pubspec.yaml in the root without .gitignore
        if self.matches_project_file(path).await? {
            if self.projects.contains_key(path) {
                return Ok(());
            }
            // read pubspec.yaml
            let pubspec_yaml = read_to_string(path).await?;
            let pubspec: yaml_serde::Value = yaml_serde::from_str(&pubspec_yaml)?;

            // Check if this is a workspace (melos workspace or similar).
            // AGENTS.md rule: all file ops via `tokio::fs`. `try_exists`
            // treats a stat error (broken symlink, permission denied) as
            // "does not exist", matching the previous sync `is_file()`
            // fallthrough on error.
            //
            // Short-circuit: when the pubspec's inline `workspace:` field
            // is already present, skip building the sibling `melos.yaml`
            // PathBuf and issuing the async `try_exists` syscall. Mirrors
            // the same optimization the node finder applies to
            // `pnpm-workspace.yaml` (retry-now#0010).
            let is_workspace = if pubspec.get("workspace").is_some() {
                true
            } else {
                let melos_yaml = path
                    .parent()
                    .with_context(|| format!("Parent not found - {}", path.display()))?
                    .join("melos.yaml");
                tokio::fs::try_exists(&melos_yaml).await.unwrap_or(false)
            };

            // Both branches use the same name/version and the same path;
            // hoist so each branch collapses to a single constructor call.
            // `path_key` / `relative_path_key` naming matches every other
            // finder (Node, Python, CSharp, Java, and post-item-2 Rust) so
            // grepping for the "shared hoisted key" idiom finds every
            // finder at once.
            //
            // Delegate the `.get(...).and_then(as_str).map(...)` chain to
            // the module-private `pubspec_str` helper — mirrors the
            // `project_str` / `package_str` sibling helpers in
            // `crates/python/src/finder.rs` and `crates/rust/src/finder.rs`
            // so the manifest-shape assumption lives in exactly one place
            // per finder. Semantically identical to the inline chain:
            // `yaml_serde::Value`'s `Index` impl returns `Value::Null` for
            // missing keys, and `Value::Null.as_str()` is `None`, so both
            // present-string and missing-field shapes round-trip
            // unchanged.
            let version = pubspec_str(&pubspec, "version");
            let name = pubspec_str(&pubspec, "name");
            let path_key = path.to_path_buf();
            let relative_path_key = relative_path.to_path_buf();

            let mut project = if is_workspace {
                Project::Workspace(Box::new(DartWorkspace::new(
                    name,
                    version,
                    path_key.clone(),
                    relative_path_key,
                )))
            } else {
                Project::Package(Box::new(DartPackage::new(
                    name,
                    version,
                    path_key.clone(),
                    relative_path_key,
                )))
            };

            // read dependencies section — track only LOCAL monorepo deps
            // (entries whose value is a mapping containing a `path:` key).
            // Bare version strings like `http: ^1.0.0` point at pub.dev and
            // cannot be resolved by `sort_by_dependencies` (which filters
            // via `name_to_index` over local project names), so tracking
            // them just burns a `HashSet` insertion + a `String` allocation
            // per external dep. This also aligns Dart with every other
            // finder: Node keeps only `workspace:*`, Python only
            // `[tool.uv.sources]`, Rust only `dep.workspace == true`, and
            // C# only `<ProjectReference Include="..." />`.
            if let Some(dependencies) = pubspec.get("dependencies").and_then(|d| d.as_mapping()) {
                for (dep_name, dep_value) in dependencies {
                    if let Some(dep_str) = dep_name.as_str()
                        && dep_value
                            .as_mapping()
                            .is_some_and(|m| m.contains_key("path"))
                    {
                        project.add_dependency(dep_str);
                    }
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
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;

    // Both `DartProjectFinder::new()` and `DartProjectFinder::default()` must
    // yield the same empty, `pubspec.yaml`-scoped finder.
    #[rstest]
    #[case(DartProjectFinder::new())]
    #[case(DartProjectFinder::default())]
    #[tokio::test]
    async fn test_construction(#[case] finder: DartProjectFinder) {
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
                // Only local (`path:`) deps are tracked — matches the
                // "only monorepo deps" invariant every other language
                // finder already honors. `http: ^1.0.0` points at
                // pub.dev, so it is intentionally excluded here.
                assert_eq!(deps.len(), 2);
                assert!(deps.contains("core"));
                assert!(deps.contains("utils"));
                assert!(!deps.contains("http"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_with_only_external_deps() {
        // Regression: a pubspec whose `dependencies:` is entirely bare
        // version strings (all external pub.dev packages) must yield an
        // empty `dependencies()` HashSet on the resulting project.
        // `sort_by_dependencies` already ignores names it cannot resolve
        // via `name_to_index`, so tracking them was pure allocation waste
        // — and made Dart the odd one out among the language finders.
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
dependencies:
  http: ^1.0.0
  path: ^1.9.0
  intl: ^0.20.0
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
                assert!(
                    pkg.dependencies().is_empty(),
                    "expected zero local deps, got {:?}",
                    pkg.dependencies()
                );
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }
}
