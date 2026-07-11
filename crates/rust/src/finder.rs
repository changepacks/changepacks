use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Package, Project, ProjectFinder, is_regular_file};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::RustPackage, workspace::RustWorkspace};

/// Package info deferred for workspace version resolution
#[derive(Debug)]
struct PendingWorkspacePackage {
    name: Option<String>,
    abs_path: PathBuf,
    relative_path: PathBuf,
    dependencies: Vec<String>,
}

/// Manifest filenames this finder recognizes. Static because the list is
/// compile-time constant — no per-instance heap `Vec` is needed and the
/// `ProjectFinder::project_files` return type (`&[&str]`) already accepts
/// a `&'static [&'static str]`.
const PROJECT_FILES: &[&str] = &["Cargo.toml"];

/// Look up `[package].<field>` as an owned string, mirroring the
/// `doc.get("package").and_then(|p| p.get(field)).and_then(|v| v.as_str()).map(String::from)`
/// chain that used to be open-coded across `visit` and `finalize`. Extracted so
/// a future manifest shape change (e.g. inline-table `name = { workspace = true }`)
/// only needs to be adapted in one place.
///
/// See [`workspace_package_str`] for the `[workspace.package].<field>` sibling.
fn package_str(doc: &toml_edit::DocumentMut, field: &str) -> Option<String> {
    doc.get("package")
        .and_then(|p| p.get(field))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Look up `[workspace.package].<field>` as an owned string, mirroring the
/// `doc.get("workspace").and_then(|w| w.get("package")).and_then(|p| p.get(field)).and_then(|v| v.as_str()).map(String::from)`
/// chain that was previously open-coded twice in this file (once in `visit`
/// to seed `workspace_package_version`, once in `finalize` to walk up to a
/// missed workspace root). Extracted so the manifest-shape assumption lives
/// in one place, matching the `[package].<field>` sibling helper.
fn workspace_package_str(doc: &toml_edit::DocumentMut, field: &str) -> Option<String> {
    doc.get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get(field))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Return `true` for a `toml_edit::Item` whose value is table-like with a
/// `path` key — the shape Cargo uses for direct local-path dependencies
/// (`dep = { path = "../dep" }`, optionally alongside `version`). Sibling
/// of [`crate::is_workspace_marker`]: local-path edges are in-repo dependencies
/// just like workspace-inherited ones, so they must feed publish ordering
/// and reverse updates too. Registry dependencies (`dep = "1.0"`) are
/// scalars, so `as_table_like()` returns `None` and they are excluded.
fn is_local_path_dep(value: &toml_edit::Item) -> bool {
    value
        .as_table_like()
        .is_some_and(|table| table.contains_key("path"))
}

/// Dependency tables Cargo can use for local package edges.
const CARGO_DEPENDENCY_TABLES: &[&str] =
    &["dependencies", "dev-dependencies", "build-dependencies"];

fn collect_workspace_dep_names_from_table<'a>(
    deps: &'a dyn toml_edit::TableLike,
    dep_names: &mut Vec<&'a str>,
) {
    for (dep_name, value) in deps.iter() {
        if crate::is_workspace_marker(value) || is_local_path_dep(value) {
            dep_names.push(dep_name);
        }
    }
}

/// Collect names of dependency entries declared as `dep = { workspace = true }`
/// — the shape used by workspace members to inherit dependency versions from
/// `[workspace.dependencies]`.
///
/// Previously open-coded inside `visit`; extracted so the same
/// `dep_names` list feeds every branch (workspace / inherits-workspace-
/// version / plain-package) through one code path. It checks top-level Cargo
/// dependency tables and target-specific dependency tables so dev, build, and
/// platform-only local edges still feed publish ordering and reverse updates.
///
/// Matches the `package_str` / `workspace_package_str` sibling-helper
/// idiom already established in this file.
fn workspace_dep_names(doc: &toml_edit::DocumentMut) -> Vec<&str> {
    let mut dep_names = Vec::new();

    for table_name in CARGO_DEPENDENCY_TABLES {
        if let Some(deps) = doc.get(table_name).and_then(toml_edit::Item::as_table_like) {
            collect_workspace_dep_names_from_table(deps, &mut dep_names);
        }
    }

    if let Some(targets) = doc.get("target").and_then(toml_edit::Item::as_table_like) {
        for (_, target) in targets.iter() {
            let Some(target_table) = target.as_table_like() else {
                continue;
            };
            for table_name in CARGO_DEPENDENCY_TABLES {
                if let Some(deps) = target_table
                    .get(table_name)
                    .and_then(toml_edit::Item::as_table_like)
                {
                    collect_workspace_dep_names_from_table(deps, &mut dep_names);
                }
            }
        }
    }

    dep_names
}

#[derive(Debug, Default)]
pub struct RustProjectFinder {
    projects: HashMap<PathBuf, Project>,
    workspace_package_version: Option<String>,
    workspace_root_path: Option<PathBuf>,
    pending_workspace_packages: Vec<PendingWorkspacePackage>,
}

impl RustProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve all pending workspace packages by creating `RustPackage` instances
    /// with the workspace version and inserting them into `self.projects`.
    /// Drains `self.pending_workspace_packages` in the process.
    fn resolve_pending_workspace_packages(&mut self) {
        let pending = std::mem::take(&mut self.pending_workspace_packages);
        for p in pending {
            let mut pkg = RustPackage::new_with_workspace_version(
                p.name,
                self.workspace_package_version.clone(),
                p.abs_path.clone(),
                p.relative_path,
                self.workspace_root_path.clone(),
            );
            for dep in &p.dependencies {
                pkg.add_dependency(dep);
            }
            self.projects
                .insert(p.abs_path, Project::Package(Box::new(pkg)));
        }
    }
}

#[async_trait]
impl ProjectFinder for RustProjectFinder {
    changepacks_core::impl_projects_hashmap_accessors!();

    fn project_files(&self) -> &[&str] {
        PROJECT_FILES
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        if !self.matches_project_file(path).await {
            return Ok(());
        }
        if self.projects.contains_key(path) {
            return Ok(());
        }
        // read Cargo.toml
        let (_cargo_toml_raw, cargo_toml) = crate::read_and_parse_cargo_toml(path).await?;

        // Collect workspace dependencies for this file — the same
        // `dep_names` list feeds every branch below (workspace /
        // inherits-workspace-version / plain-package).
        let dep_names = workspace_dep_names(&cargo_toml);

        // if workspace
        if cargo_toml.get("workspace").is_some() {
            // Read [workspace.package].version if present
            let ws_pkg_version = workspace_package_str(&cargo_toml, "version");
            if ws_pkg_version.is_some() {
                self.workspace_package_version = ws_pkg_version;
                self.workspace_root_path = Some(path.to_path_buf());
            }

            // A visited workspace root's own version: prefer its `[package].version`
            // string, but fall back to `[workspace.package].version` for a virtual
            // workspace (no `[package]`) or a hybrid root whose `[package]` inherits
            // via `version.workspace = true` (a table, so `package_str` → `None`).
            // Without this fallback the constructed `RustWorkspace` reports
            // `version = None`, and a later inherited bump promoted onto the root
            // path would rewrite from `0.0.0`, downgrading the real version. This
            // aligns `visit` with the same fallback `finalize` already applies.
            let version = package_str(&cargo_toml, "version")
                .or_else(|| workspace_package_str(&cargo_toml, "version"));
            let name = package_str(&cargo_toml, "name");
            // Hoist the shared `PathBuf` into one binding: `path_key` seeds
            // both the `RustWorkspace::new(...)` constructor slot and the
            // `self.projects.insert(...)` map key. Mirror of the same
            // pattern already used by the `inherits_workspace` and
            // plain-package `else` arms below, and by
            // `crates/csharp/src/finder.rs::visit` /
            // `crates/java/src/finder.rs::visit`. Byte-identical
            // semantics — the same `PathBuf` bytes flow into
            // `RustWorkspace::new` and the map key, just materialized
            // once up front.
            let path_key = path.to_path_buf();
            let mut project = Project::Workspace(Box::new(RustWorkspace::new(
                name,
                version,
                path_key.clone(),
                relative_path.to_path_buf(),
            )));
            for &dep_name in &dep_names {
                project.add_dependency(dep_name);
            }
            self.projects.insert(path_key, project);

            // Resolve any pending packages that were visited before this workspace
            self.resolve_pending_workspace_packages();
        } else {
            // Check if version.workspace = true — same table-like +
            // `workspace = true` shape as `workspace_dep_names`
            // above, so both call sites share the [`is_workspace_marker`]
            // decoder. Byte-identical to the previous
            // six-`.and_then` chain because `is_some_and(...)`
            // short-circuits on the same `None` cases and its final
            // `.unwrap_or(false)` matches.
            let inherits_workspace = cargo_toml
                .get("package")
                .and_then(|p| p.get("version"))
                .is_some_and(crate::is_workspace_marker);

            let name = package_str(&cargo_toml, "name");

            // Hoist BOTH shared `PathBuf`s once for every non-workspace
            // arm: `path_key` / `relative_path_key` seed both the
            // constructor slot (`RustPackage::new*` /
            // `PendingWorkspacePackage`) AND the
            // `self.projects.insert(...)` map key (for `path_key`),
            // mirroring the same pattern already used in the
            // workspace arm above and by every peer finder (Node,
            // Python, CSharp, Java, Dart). Each branch clones each
            // key into non-final slots and moves it into the LAST-
            // used slot — one `PathBuf` allocation per key per visit
            // instead of two-to-three, byte-identical output.
            let path_key = path.to_path_buf();
            let relative_path_key = relative_path.to_path_buf();

            if inherits_workspace {
                if self.workspace_package_version.is_some() {
                    // Workspace already visited — resolve immediately
                    let mut pkg = RustPackage::new_with_workspace_version(
                        name,
                        self.workspace_package_version.clone(),
                        path_key.clone(),
                        relative_path_key,
                        self.workspace_root_path.clone(),
                    );
                    for &dep_name in &dep_names {
                        pkg.add_dependency(dep_name);
                    }
                    self.projects
                        .insert(path_key, Project::Package(Box::new(pkg)));
                } else {
                    // Workspace not yet visited — defer
                    self.pending_workspace_packages
                        .push(PendingWorkspacePackage {
                            name,
                            abs_path: path_key,
                            relative_path: relative_path_key,
                            dependencies: dep_names.iter().map(|s| s.to_string()).collect(),
                        });
                }
            } else {
                let version = package_str(&cargo_toml, "version");
                let mut project = Project::Package(Box::new(RustPackage::new(
                    name,
                    version,
                    path_key.clone(),
                    relative_path_key,
                )));
                for &dep_name in &dep_names {
                    project.add_dependency(dep_name);
                }
                self.projects.insert(path_key, project);
            }
        }
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        // If workspace root was never visited (e.g. excluded by ignore patterns),
        // walk up from the first pending package to find and read it.
        //
        // The `!pending_workspace_packages.is_empty()` guard that used to sit
        // between `workspace_package_version.is_none()` and the `let Some(...)
        // = ..first()` bind has been dropped: `Vec::first()` returns `None`
        // iff the vec is empty, so the following `let-else` bind is a strict
        // superset of the emptiness check. One fewer condition, byte-identical
        // control flow.
        if self.workspace_package_version.is_none()
            && let Some(first_pkg) = self.pending_workspace_packages.first()
        {
            // Derive git root from the first pending package's absolute/relative
            // paths: strip `rel_component_count` trailing components from
            // `abs_path`. `Path::ancestors()` already exposes that walk as an
            // iterator (matching the `abs_path.ancestors().skip(2)` walk-up
            // used a few lines below), and `nth(n)` yields the same result as
            // `n` sequential `PathBuf::pop()` calls without cloning + mutating
            // the buffer in a for-loop. The `unwrap_or_else` fallback
            // preserves the previous behaviour for the pathological edge case
            // where the count exceeds the ancestor depth: the old pop loop
            // stopped at `""`, and the safe cousin `first_pkg.abs_path.clone()`
            // still yields the same lookup result downstream because
            // `strip_prefix(&git_root)` in the subsequent block just falls
            // back to `Path::new("Cargo.toml")`.
            let git_root = first_pkg
                .abs_path
                .ancestors()
                .nth(first_pkg.relative_path.components().count())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| first_pkg.abs_path.clone());

            // `Path::ancestors()` yields `[self, parent, grandparent, …, root]`,
            // so `skip(2)` starts at grandparent — the same starting point as
            // the previous `first_pkg.abs_path.parent().and_then(Path::parent)`
            // seed. Using the iterator lets the walk-up terminate cleanly with
            // a plain `break` instead of a mutable `dir` slot juggled by hand.
            for parent in first_pkg.abs_path.ancestors().skip(2) {
                // Bound the fallback walk to the `git_root` computed above:
                // never climb PAST the repository root and adopt an out-of-repo
                // `Cargo.toml` (e.g. a parent Rust project this repo is nested
                // inside), which would silently rewrite inherited-version
                // resolution for every member. `ancestors()` still yields
                // `git_root` itself — `starts_with` is true there, so the real
                // root stays reachable — then its parents, where `starts_with`
                // turns false and the walk stops. Mirrors the git-scoped bound
                // the C# finder applies in `is_workspace`
                // (`parent.ancestors().take(max_depth)` in
                // `crates/csharp/src/finder.rs`).
                if !parent.starts_with(&git_root) {
                    break;
                }
                let candidate = parent.join("Cargo.toml");
                // AGENTS.md rule: all file ops via `tokio::fs`. A stat error
                // is treated as "does not exist", matching the previous
                // sync `is_file()` fallthrough on error. Delegated to the
                // shared `changepacks_core::is_regular_file` helper so the
                // same "stat, coerce error to false" shape lives in ONE
                // place — this is the same import + call the CSharp finder
                // already uses (`crates/csharp/src/finder.rs:3` + call site
                // in its own `finalize`).
                if is_regular_file(&candidate).await
                    && let Ok(content) = read_to_string(&candidate).await
                    && let Ok(parsed) = content.parse::<toml_edit::DocumentMut>()
                    && let Some(version) = workspace_package_str(&parsed, "version")
                {
                    // Hoist `candidate` into a `root_path` local so both the
                    // `self.workspace_root_path` write and the `self.projects`
                    // insert key resolve to the same value without re-`clone()
                    // .unwrap()`ing back out of `workspace_root_path`. Retires
                    // one `.unwrap()` — the insert key was previously
                    // `self.workspace_root_path.clone().unwrap()` on the value
                    // we already have in hand as `candidate`. Byte-identical
                    // semantics: the map key inserted into `self.projects` and
                    // the path stored in `self.workspace_root_path` remain the
                    // same `candidate` value.
                    let root_path = candidate;
                    self.workspace_package_version = Some(version);
                    self.workspace_root_path = Some(root_path.clone());

                    // Insert synthetic workspace project so apply_updates() can find it
                    let ws_name = package_str(&parsed, "name");
                    let ws_pkg_version = package_str(&parsed, "version");
                    let ws_relative_path = root_path
                        .strip_prefix(&git_root)
                        .unwrap_or(Path::new("Cargo.toml"))
                        .to_path_buf();

                    let workspace = RustWorkspace::new(
                        ws_name,
                        // For virtual workspaces (no [package]), use [workspace.package].version
                        ws_pkg_version.or_else(|| self.workspace_package_version.clone()),
                        root_path.clone(),
                        ws_relative_path,
                    );
                    self.projects
                        .insert(root_path, Project::Workspace(Box::new(workspace)));
                    break;
                }
            }
        }

        self.resolve_pending_workspace_packages();
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

    // Both `RustProjectFinder::new()` and `RustProjectFinder::default()` must
    // yield the same empty, `Cargo.toml`-scoped finder.
    #[rstest]
    #[case(RustProjectFinder::new())]
    #[case(RustProjectFinder::default())]
    fn test_rust_project_finder_construction(#[case] finder: RustProjectFinder) {
        assert_eq!(finder.project_files(), &["Cargo.toml"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
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
    async fn test_rust_project_finder_visit_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[package]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
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
    async fn test_rust_project_finder_visit_workspace_without_package() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
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
    async fn test_rust_project_finder_visit_workspace_uses_workspace_package_version() {
        // A virtual workspace root (no [package]) that declares its version only
        // via [workspace.package].version. When VISITED directly, the finder must
        // report that version on the Workspace project — mirroring the fallback
        // finalize() already applies — so a later inherited bump promoted onto the
        // root path never rewrites from a phantom 0.0.0 and downgrades it.
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.33"
edition = "2024"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Workspace(ws) => {
                // No [package] name, but the version is inherited from
                // [workspace.package].version via the new fallback.
                assert_eq!(ws.name(), None);
                assert_eq!(ws.version(), Some("0.1.33"));
            }
            _ => panic!("Expected Workspace"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_non_cargo_file() {
        let temp_dir = TempDir::new().unwrap();
        let other_file = temp_dir.path().join("other.txt");
        fs::write(&other_file, "some content").unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&other_file, &PathBuf::from("other.txt"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_directory() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        // Pass directory instead of file
        finder
            .visit(temp_dir.path(), &PathBuf::from("."))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 0);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        // Visit again - should not add duplicate
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        assert_eq!(finder.projects().len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_multiple_packages() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml1 = temp_dir.path().join("package1").join("Cargo.toml");
        fs::create_dir_all(cargo_toml1.parent().unwrap()).unwrap();
        fs::write(
            &cargo_toml1,
            r#"[package]
name = "package1"
version = "1.0.0"
"#,
        )
        .unwrap();

        let cargo_toml2 = temp_dir.path().join("package2").join("Cargo.toml");
        fs::create_dir_all(cargo_toml2.parent().unwrap()).unwrap();
        fs::write(
            &cargo_toml2,
            r#"[package]
name = "package2"
version = "2.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml1, &PathBuf::from("package1/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&cargo_toml2, &PathBuf::from("package2/Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_projects_mut() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let mut_projects = finder.projects_mut();
        assert_eq!(mut_projects.len(), 1);

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_with_workspace_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
core = { workspace = true }
utils = { workspace = true }
external = "1.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                assert_eq!(pkg.name(), Some("test-package"));
                let deps = pkg.dependencies();
                assert_eq!(deps.len(), 2);
                assert!(deps.contains("core"));
                assert!(deps.contains("utils"));
                // external is not a workspace dependency
                assert!(!deps.contains("external"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_with_path_dependencies() {
        // Given: a package manifest with a direct local-path dependency and a
        // registry dependency
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
foo = { path = "../foo", version = "0.1" }
external = "1.0"
"#,
        )
        .unwrap();

        // When: the finder visits the manifest
        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        // Then: the path dependency is tracked, the registry one is not
        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                let deps = pkg.dependencies();
                assert_eq!(deps.len(), 1, "expected only the path dep, got {deps:?}");
                assert!(deps.contains("foo"));
                assert!(!deps.contains("external"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_with_workspace_dependencies_from_all_cargo_sections()
     {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"

[dependencies]
runtime-core = { workspace = true }
external = "1.0"

[dev-dependencies]
test-support = { workspace = true }
tempfile = "3"

[build-dependencies]
build-helper = { workspace = true }
cc = "1"

[target.'cfg(unix)'.dependencies]
unix-support = { workspace = true }
libc = "0.2"

[target.'cfg(windows)'.dev-dependencies]
windows-test-support = { workspace = true }

[target.'cfg(target_arch = "wasm32")'.build-dependencies]
wasm-build-helper = { workspace = true }
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 1);
        match projects[0] {
            Project::Package(pkg) => {
                let deps = pkg.dependencies();
                assert_eq!(deps.len(), 6, "expected all workspace deps, got {deps:?}");
                assert!(deps.contains("runtime-core"));
                assert!(deps.contains("test-support"));
                assert!(deps.contains("build-helper"));
                assert!(deps.contains("unix-support"));
                assert!(deps.contains("windows-test-support"));
                assert!(deps.contains("wasm-build-helper"));
                assert!(!deps.contains("external"));
                assert!(!deps.contains("tempfile"));
                assert!(!deps.contains("cc"));
                assert!(!deps.contains("libc"));
            }
            _ => panic!("Expected Package"),
        }

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_virtual_workspace_with_workspace_version() {
        // Reproduces vespera-style virtual workspace (no [package] section)
        let temp_dir = TempDir::new().unwrap();

        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.33"
edition = "2024"
"#,
        )
        .unwrap();

        let pkg_dir = temp_dir.path().join("crates").join("vespera");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_toml = pkg_dir.join("Cargo.toml");
        fs::write(
            &pkg_toml,
            r#"[package]
name = "vespera"
version.workspace = true
edition.workspace = true

[dependencies]
vespera_core = { workspace = true }

[lints]
workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&workspace_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&pkg_toml, &PathBuf::from("crates/vespera/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        // Virtual workspace (no [package]) + 1 member
        assert_eq!(projects.len(), 2);

        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("vespera"))
            .unwrap();
        assert_eq!(pkg.version(), Some("0.1.33"));
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_with_workspace_version() {
        let temp_dir = TempDir::new().unwrap();

        // Create workspace root
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "2.5.0"
edition = "2024"

[package]
name = "my-workspace"
version = "2.5.0"
"#,
        )
        .unwrap();

        // Create member package with version.workspace = true
        let pkg_dir = temp_dir.path().join("crates").join("my-crate");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_toml = pkg_dir.join("Cargo.toml");
        fs::write(
            &pkg_toml,
            r#"[package]
name = "my-crate"
version.workspace = true
edition.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        // Visit workspace first (normal git index order)
        finder
            .visit(&workspace_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&pkg_toml, &PathBuf::from("crates/my-crate/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        // Find the package
        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("my-crate"))
            .unwrap();
        assert_eq!(pkg.version(), Some("2.5.0")); // Should inherit workspace version
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_package_before_workspace() {
        let temp_dir = TempDir::new().unwrap();

        // Create workspace root
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "3.0.0"

[package]
name = "my-workspace"
version = "3.0.0"
"#,
        )
        .unwrap();

        // Create member package
        let pkg_dir = temp_dir.path().join("crates").join("my-crate");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_toml = pkg_dir.join("Cargo.toml");
        fs::write(
            &pkg_toml,
            r#"[package]
name = "my-crate"
version.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        // Visit package BEFORE workspace (reverse order)
        finder
            .visit(&pkg_toml, &PathBuf::from("crates/my-crate/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&workspace_toml, &PathBuf::from("Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("my-crate"))
            .unwrap();
        assert_eq!(pkg.version(), Some("3.0.0")); // Should still resolve correctly
    }

    #[tokio::test]
    async fn test_rust_project_finder_workspace_ignored_by_config() {
        // Simulates when ignore patterns like ["**", "!crates/**"] skip the root Cargo.toml
        let temp_dir = TempDir::new().unwrap();

        // Create workspace root (won't be visited due to ignore)
        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.33"
edition = "2024"
"#,
        )
        .unwrap();

        // Create 2 member packages
        for name in ["vespera", "vespera_core"] {
            let pkg_dir = temp_dir.path().join("crates").join(name);
            fs::create_dir_all(&pkg_dir).unwrap();
            fs::write(
                pkg_dir.join("Cargo.toml"),
                format!(
                    r#"[package]
name = "{name}"
version.workspace = true
edition.workspace = true

[lints]
workspace = true
"#
                ),
            )
            .unwrap();
        }

        let mut finder = RustProjectFinder::new();
        // Only visit member packages (workspace root is ignored)
        for name in ["vespera", "vespera_core"] {
            let pkg_toml = temp_dir.path().join("crates").join(name).join("Cargo.toml");
            finder
                .visit(
                    &pkg_toml,
                    &PathBuf::from(format!("crates/{name}/Cargo.toml")),
                )
                .await
                .unwrap();
        }
        // finalize should discover the workspace root by walking up
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        // 2 member packages + 1 synthetic workspace
        assert_eq!(projects.len(), 3);

        for name in ["vespera", "vespera_core"] {
            let pkg = projects.iter().find(|p| p.name() == Some(name)).unwrap();
            assert_eq!(
                pkg.version(),
                Some("0.1.33"),
                "{name} should inherit workspace version"
            );
        }

        // Synthetic workspace should exist with the workspace version
        let ws = projects
            .iter()
            .find(|p| matches!(p, Project::Workspace(_)))
            .expect("synthetic workspace should be created");
        assert_eq!(ws.version(), Some("0.1.33"));
        assert_eq!(ws.relative_path(), Path::new("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_discovers_workspace_with_package_section() {
        // When finalize() walks up to discover the workspace root, and that root
        // has a [package] section with name and version, lines 162-163 return Some(...)
        let temp_dir = TempDir::new().unwrap();

        let workspace_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.2.0"

[package]
name = "my-workspace-root"
version = "0.2.0"
"#,
        )
        .unwrap();

        let pkg_dir = temp_dir.path().join("crates").join("my-crate");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(
            pkg_dir.join("Cargo.toml"),
            r#"[package]
name = "my-crate"
version.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        // Only visit member (workspace root is NOT visited — simulates ignore config)
        let pkg_toml = pkg_dir.join("Cargo.toml");
        finder
            .visit(&pkg_toml, &PathBuf::from("crates/my-crate/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        assert_eq!(projects.len(), 2);

        let ws = projects
            .iter()
            .find(|p| matches!(p, Project::Workspace(_)))
            .unwrap();
        assert_eq!(ws.name(), Some("my-workspace-root"));
        assert_eq!(ws.version(), Some("0.2.0"));

        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("my-crate"))
            .unwrap();
        assert_eq!(pkg.version(), Some("0.2.0"));
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_ignores_cargo_toml_above_git_root() {
        // Regression: when the workspace root is NOT visited (e.g. excluded by
        // ignore patterns), finalize() walks up from the first pending member
        // looking for a `Cargo.toml` carrying `[workspace.package].version`.
        // That walk must be BOUNDED to the git root — it must never climb past
        // the repository root and adopt an out-of-repo `Cargo.toml` (e.g. a
        // parent Rust project this repo is nested inside), which would silently
        // rewrite inherited-version resolution for every member. Mirrors the
        // C# finder's `test_visit_package_ignores_sln_above_repo_root`.
        let temp_dir = TempDir::new().unwrap();

        // Decoy workspace root ABOVE the simulated repo root. If the walk were
        // unbounded it would climb here and adopt version "9.9.9".
        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["repo/crates/*"]

[workspace.package]
version = "9.9.9"
edition = "2024"
"#,
        )
        .unwrap();

        // Simulated repo root at <temp>/repo — deliberately has NO Cargo.toml,
        // so the bounded walk finds nothing in-repo and must stop at the root
        // instead of escaping to the decoy above it. The member sits two levels
        // below, and its relative path (3 components) pins the git root to
        // <temp>/repo via the `ancestors().nth(components)` derivation.
        let member_dir = temp_dir.path().join("repo").join("crates").join("mycrate");
        fs::create_dir_all(&member_dir).unwrap();
        let member_toml = member_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "mycrate"
version.workspace = true
edition.workspace = true
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, &PathBuf::from("crates/mycrate/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        let projects = finder.projects();
        // No synthetic workspace is adopted from the out-of-repo decoy, so the
        // member stays the only project.
        assert_eq!(
            projects.len(),
            1,
            "a Cargo.toml above the git root must not be adopted as the workspace"
        );
        assert!(
            !projects.iter().any(|p| matches!(p, Project::Workspace(_))),
            "no synthetic workspace should be created from an out-of-repo Cargo.toml"
        );

        let pkg = projects
            .iter()
            .find(|p| p.name() == Some("mycrate"))
            .expect("member package should exist");
        assert_ne!(
            pkg.version(),
            Some("9.9.9"),
            "member must not inherit the decoy workspace version from above the repo root"
        );
        assert_eq!(
            pkg.version(),
            None,
            "with no in-repo workspace root found, the member version stays unresolved"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_visit_malformed_cargo_toml() {
        // Given: a malformed Cargo.toml file
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, "invalid toml [[[").unwrap();

        // When: visit is called on the malformed manifest
        let mut finder = RustProjectFinder::new();
        let result = finder
            .visit(&cargo_toml, &PathBuf::from("Cargo.toml"))
            .await;

        // Then: the error includes both the manifest path and "Failed to parse Cargo.toml"
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to parse Cargo.toml"),
            "error message should contain 'Failed to parse Cargo.toml', got: {err_msg}"
        );
        assert!(
            err_msg.contains(cargo_toml.to_string_lossy().as_ref()),
            "error message should contain the manifest path, got: {err_msg}"
        );

        temp_dir.close().unwrap();
    }
}
