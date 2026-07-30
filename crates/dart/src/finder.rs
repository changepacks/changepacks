use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::{package::DartPackage, workspace::DartWorkspace};

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["pubspec.yaml"];

/// The pubspec.yaml sections scanned for dependency candidate names.
/// Graph consumers later intersect these names with the discovered projects.
const PUBSPEC_DEPENDENCY_SECTIONS: &[&str] =
    &["dependencies", "dev_dependencies", "dependency_overrides"];

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
        Self::default()
    }
}

/// Collect every dependency name from the supported pubspec sections onto
/// `project`. Dart workspace packages commonly use ordinary version
/// constraints rather than `path:` mappings; graph consumers resolve these
/// names against the complete project-name set and ignore unmatched external
/// packages. Extracted from `visit` to mirror the identically named helper in
/// `crates/node/src/finder.rs`, so the "walk the manifest's dependency
/// sections" step is a named call site in every `HashMap`-backed finder.
fn add_workspace_dependencies(project: &mut Project, pubspec: &yaml_serde::Value) {
    for section in PUBSPEC_DEPENDENCY_SECTIONS {
        if let Some(dependencies) = pubspec.get(*section).and_then(|d| d.as_mapping()) {
            for dep_name in dependencies.keys() {
                if let Some(dep_str) = dep_name.as_str() {
                    project.add_dependency(dep_str);
                }
            }
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
        // Parse this manifest if it is a recognized project file not already
        // visited. Both guards live in `ProjectFinder::should_visit_manifest`
        // (name/stat gate first, already-discovered map probe second) so the
        // prelude is written once for every file-name-based finder.
        if !self.should_visit_manifest(path).await? {
            return Ok(());
        }
        // read + parse pubspec.yaml through the shared head
        // `changepacks_utils::read_and_parse` (the mirror of
        // `write_finalized`), which attaches the `Failed to read pubspec.yaml
        // <path>` / `Failed to parse pubspec.yaml <path>` contexts. The raw
        // text is unused here — only the finder's write path replays it.
        let (_pubspec_yaml, pubspec): (String, yaml_serde::Value) =
            changepacks_utils::read_and_parse(path, "pubspec.yaml", |raw| {
                yaml_serde::from_str(raw)
            })
            .await?;

        // Check if this is a workspace (melos workspace or similar).
        // Short-circuit: when the pubspec's inline `workspace:` field is a
        // sequence, skip the sibling `melos.yaml` stat. Shared with
        // the Node finder via `changepacks_utils::is_workspace_by_sibling`
        // — the one source of truth for the "declared field OR fixed
        // sibling file" policy (stat error → not-a-workspace; all file ops
        // via `tokio::fs`).
        //
        // The absent/valid/invalid triage itself is
        // `changepacks_utils::ensure_declared_shape`, shared verbatim with the
        // Node and Python finders; only the shape predicate stays here because
        // it is the one part that speaks `yaml_serde::Value`.
        let has_workspace_declaration = changepacks_utils::ensure_declared_shape(
            pubspec.get("workspace").map(yaml_serde::Value::is_sequence),
            path,
            "workspace",
            "a sequence",
        )?;
        let is_workspace = changepacks_utils::is_workspace_by_sibling(
            has_workspace_declaration,
            path,
            "melos.yaml",
        )
        .await?;

        // Both branches use the same name/version and the same path;
        // hoist so each branch collapses to a single constructor call.
        // `path_key` / `relative_path_key` naming matches every other
        // finder (Node, Python, CSharp, Java, and Rust) so
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
        // Mirrors the Node finder's borrow-only `private` gate.
        let publishable_by_default = pubspec
            .get("publish_to")
            .and_then(|v| v.as_str())
            .is_none_or(|publish_to| publish_to.trim() != "none");
        let path_key = path.to_path_buf();
        let relative_path_key = relative_path.to_path_buf();

        let mut project = if is_workspace {
            Project::Workspace(Box::new(DartWorkspace::new_discovered(
                name,
                version,
                path_key.clone(),
                relative_path_key,
                publishable_by_default,
            )))
        } else {
            Project::Package(Box::new(DartPackage::new_discovered(
                name,
                version,
                path_key.clone(),
                relative_path_key,
                publishable_by_default,
            )))
        };

        add_workspace_dependencies(&mut project, &pubspec);

        self.projects.insert(path_key, project);
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

    async fn discover_single_pubspec(contents: &str) -> (TempDir, DartProjectFinder) {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(&pubspec_path, contents).unwrap();

        let mut finder = DartProjectFinder::new();
        finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await
            .unwrap();

        (temp_dir, finder)
    }

    #[rstest]
    #[case("publish_to: none\n", false)]
    #[case("publish_to: 'none'\n", false)]
    #[case("publish_to: \" none \"\n", false)]
    #[case("publish_to: https://packages.example.com\n", true)]
    #[case("", true)]
    #[case("publish_to: false\n", true)]
    #[tokio::test]
    async fn test_package_publish_to_controls_default_publishability(
        #[case] publish_to: &str,
        #[case] expected: bool,
    ) {
        let pubspec = format!("name: test_package\nversion: 1.0.0\n{publish_to}");
        let (_temp_dir, finder) = discover_single_pubspec(&pubspec).await;
        let projects = finder.projects();

        assert_eq!(projects.len(), 1);
        assert!(matches!(projects[0], Project::Package(_)));
        assert_eq!(projects[0].is_publishable_by_default(), expected);
    }

    #[rstest]
    #[case("publish_to: none\n", false)]
    #[case("publish_to: \"none\"\n", false)]
    #[case("publish_to: https://packages.example.com\n", true)]
    #[case("", true)]
    #[case("publish_to:\n  registry: private\n", true)]
    #[tokio::test]
    async fn test_workspace_publish_to_controls_default_publishability(
        #[case] publish_to: &str,
        #[case] expected: bool,
    ) {
        let pubspec = format!("name: test_workspace\nversion: 1.0.0\nworkspace: []\n{publish_to}");
        let (_temp_dir, finder) = discover_single_pubspec(&pubspec).await;
        let projects = finder.projects();

        assert_eq!(projects.len(), 1);
        assert!(matches!(projects[0], Project::Workspace(_)));
        assert_eq!(projects[0].is_publishable_by_default(), expected);
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

    #[rstest]
    #[case("workspace:\n  - packages/*\n")]
    #[case("workspace: []\n")]
    #[tokio::test]
    async fn test_visit_workspace_with_workspace_sequence(#[case] workspace: &str) {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            format!(
                r#"name: test_workspace
version: 1.0.0
{workspace}"#
            ),
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

    #[rstest]
    #[case("workspace: null\n")]
    #[case("workspace: packages/*\n")]
    #[case("workspace:\n  packages:\n    - packages/*\n")]
    #[tokio::test]
    async fn test_rejects_invalid_workspace_declaration(#[case] workspace: &str) {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            format!(
                r#"name: test_package
version: 1.0.0
{workspace}"#
            ),
        )
        .unwrap();

        let mut finder = DartProjectFinder::new();
        let result = finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await;

        let error_msg = result
            .expect_err("invalid workspace declaration should fail")
            .to_string();
        assert!(
            error_msg.contains("Invalid `workspace` declaration"),
            "error message should explain the invalid declaration, got: {error_msg}"
        );
        assert!(
            error_msg.contains(pubspec_path.to_string_lossy().as_ref()),
            "error message should contain the manifest path, got: {error_msg}"
        );
        assert!(finder.projects().is_empty());

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
                assert!(deps.contains("core"));
                assert!(deps.contains("utils"));
                assert!(deps.contains("http"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_with_only_external_deps() {
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
                assert_eq!(pkg.dependencies().len(), 3);
                assert!(pkg.dependencies().contains("http"));
                assert!(pkg.dependencies().contains("path"));
                assert!(pkg.dependencies().contains("intl"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_with_dev_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
dev_dependencies:
  test_utils:
    path: ../test_utils
  lints: ^3.0.0
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
                assert_eq!(deps.len(), 2);
                assert!(deps.contains("test_utils"));
                assert!(deps.contains("lints"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_package_with_dependency_overrides() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
dependencies:
  core: ^1.0.0
  http: ^1.0.0
dependency_overrides:
  core:
    path: ../core
  other: ^2.0.0
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
                assert!(deps.contains("core"));
                assert!(deps.contains("other"));
                assert!(deps.contains("http"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_version_dependencies_feed_local_graphs_and_ignore_unmatched_names() {
        use changepacks_core::{ChangePackResultLog, UpdateType};
        use changepacks_utils::{apply_reverse_dependencies, sort_by_dependencies};
        use std::collections::HashMap;

        let temp_dir = TempDir::new().unwrap();
        let manifests = [
            ("foo", "name: foo\nversion: 1.0.0\n"),
            ("path_dep", "name: path_dep\nversion: 1.0.0\n"),
            ("override_dep", "name: override_dep\nversion: 1.0.0\n"),
            (
                "app",
                r#"name: app
version: 1.0.0
dependencies:
  foo: ^1.0.0
  path_dep:
    path: ../path_dep
  unmatched_external: ^9.0.0
dependency_overrides:
  override_dep:
    path: ../override_dep
"#,
            ),
        ];

        let mut finder = DartProjectFinder::new();
        for (directory, contents) in manifests {
            let path = temp_dir.path().join(directory).join("pubspec.yaml");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            finder
                .visit(&path, &PathBuf::from(directory).join("pubspec.yaml"))
                .await
                .unwrap();
        }

        let projects = finder.projects();
        let by_name = |name: &str| {
            *projects
                .iter()
                .find(|project| project.name() == Some(name))
                .unwrap()
        };
        let app = by_name("app");
        assert_eq!(app.dependencies().len(), 4);
        assert!(app.dependencies().contains("foo"));
        assert!(app.dependencies().contains("path_dep"));
        assert!(app.dependencies().contains("override_dep"));
        assert!(app.dependencies().contains("unmatched_external"));

        let sorted = sort_by_dependencies(vec![
            app,
            by_name("foo"),
            by_name("path_dep"),
            by_name("override_dep"),
        ])
        .expect("fixture graph is a DAG");
        let sorted_names: Vec<_> = sorted
            .iter()
            .map(|project| project.name().unwrap())
            .collect();
        assert_eq!(sorted_names.last(), Some(&"app"));

        // The unmatched external name creates no additional edge; only the
        // matching `foo` name moves ahead of `app`.
        let sorted_without_matching_external =
            sort_by_dependencies(vec![app, by_name("foo")]).expect("fixture graph is a DAG");
        assert_eq!(sorted_without_matching_external[0].name(), Some("foo"));

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("foo").join("pubspec.yaml"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update foo".to_string(),
                )],
            ),
        );
        apply_reverse_dependencies(&mut update_map, &projects, temp_dir.path()).unwrap();
        assert_eq!(
            update_map[&PathBuf::from("app").join("pubspec.yaml")].0,
            UpdateType::Patch
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_visit_malformed_pubspec_yaml() {
        // Regression: a malformed pubspec.yaml must produce an error
        // that includes both the manifest path and the "Failed to parse
        // pubspec.yaml" context message.
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(&pubspec_path, "invalid: yaml: content: [").unwrap();

        let mut finder = DartProjectFinder::new();
        let result = finder
            .visit(&pubspec_path, &PathBuf::from("pubspec.yaml"))
            .await;

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Failed to parse pubspec.yaml"),
            "error message should contain 'Failed to parse pubspec.yaml', got: {error_msg}"
        );
        assert!(
            error_msg.contains(pubspec_path.to_string_lossy().as_ref()),
            "error message should contain the manifest path, got: {error_msg}"
        );

        temp_dir.close().unwrap();
    }
}
