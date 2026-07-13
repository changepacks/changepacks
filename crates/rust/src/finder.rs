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

/// Resolve the package name represented by a Cargo dependency entry.
///
/// Cargo dependency keys may be aliases (`alias = { package = "real-name", ... }`).
/// In that case graph edges and workspace version updates must bind to the
/// package named by `package`; ordinary dependencies continue to use their key.
pub(crate) fn effective_dependency_name<'a>(
    dependency_key: &'a str,
    value: &'a toml_edit::Item,
) -> &'a str {
    value
        .as_table_like()
        .and_then(|dependency| dependency.get("package"))
        .and_then(toml_edit::Item::as_str)
        .unwrap_or(dependency_key)
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
            dep_names.push(effective_dependency_name(dep_name, value));
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

fn workspace_dependency_aliases(doc: &toml_edit::DocumentMut) -> HashMap<String, String> {
    doc.get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml_edit::Item::as_table_like)
        .map(|dependencies| {
            dependencies
                .iter()
                .filter(|(_, dependency)| is_local_path_dep(dependency))
                .filter_map(|(dependency_key, dependency)| {
                    let package_name = effective_dependency_name(dependency_key, dependency);
                    (package_name != dependency_key)
                        .then(|| (dependency_key.to_string(), package_name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Default)]
pub struct RustProjectFinder {
    projects: HashMap<PathBuf, Project>,
    workspace_package_versions: HashMap<PathBuf, String>,
    workspace_dependency_aliases: HashMap<PathBuf, HashMap<String, String>>,
    pending_workspace_packages: Vec<PendingWorkspacePackage>,
}

impl RustProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn nearest_workspace_package(&self, member_path: &Path) -> Option<(String, PathBuf)> {
        self.workspace_package_versions
            .iter()
            .filter(|(root_path, _)| {
                root_path
                    .parent()
                    .is_some_and(|root_dir| member_path.starts_with(root_dir))
            })
            .max_by_key(|(root_path, _)| root_path.components().count())
            .map(|(root_path, version)| (version.clone(), root_path.clone()))
    }

    fn nearest_workspace_dependency_aliases(
        &self,
        member_path: &Path,
    ) -> Option<&HashMap<String, String>> {
        self.workspace_dependency_aliases
            .iter()
            .filter(|(root_path, _)| {
                root_path
                    .parent()
                    .is_some_and(|root_dir| member_path.starts_with(root_dir))
            })
            .max_by_key(|(root_path, _)| root_path.components().count())
            .map(|(_, aliases)| aliases)
    }

    async fn discover_workspace_dependency_aliases_for_member(
        &mut self,
        member_path: &Path,
        relative_path: &Path,
    ) {
        let repository_root = member_path
            .ancestors()
            .nth(relative_path.components().count())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| member_path.to_path_buf());
        let Some(mut ancestor) = member_path
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
        else {
            return;
        };

        loop {
            if !ancestor.starts_with(&repository_root) {
                return;
            }
            let candidate = ancestor.join("Cargo.toml");
            if self.workspace_dependency_aliases.contains_key(&candidate) {
                return;
            }
            if is_regular_file(&candidate).await
                && let Ok((_, parsed)) = crate::read_and_parse_cargo_toml(&candidate).await
                && parsed.get("workspace").is_some()
            {
                self.workspace_dependency_aliases
                    .insert(candidate, workspace_dependency_aliases(&parsed));
                return;
            }
            let Some(parent) = ancestor.parent() else {
                return;
            };
            ancestor = parent.to_path_buf();
        }
    }

    fn insert_workspace_member(
        &mut self,
        package: PendingWorkspacePackage,
        workspace_package_version: Option<String>,
        workspace_root_path: Option<PathBuf>,
    ) {
        let PendingWorkspacePackage {
            name,
            abs_path,
            relative_path,
            mut dependencies,
        } = package;
        if let Some(root_path) = workspace_root_path.as_ref()
            && let Some(aliases) = self.workspace_dependency_aliases.get(root_path)
        {
            for dependency in &mut dependencies {
                if let Some(package_name) = aliases.get(dependency) {
                    dependency.clone_from(package_name);
                }
            }
        }
        let mut pkg = RustPackage::new_with_workspace_version(
            name,
            workspace_package_version,
            abs_path.clone(),
            relative_path,
            workspace_root_path,
        );
        for dependency in dependencies {
            pkg.add_dependency(&dependency);
        }
        self.projects
            .insert(abs_path, Project::Package(Box::new(pkg)));
    }

    fn resolve_pending_workspace_packages_for_root(&mut self, workspace_root_path: &Path) {
        let pending = std::mem::take(&mut self.pending_workspace_packages);
        for package in pending {
            let workspace_package = self.nearest_workspace_package(&package.abs_path);
            if let Some((version, root_path)) = workspace_package
                && root_path == workspace_root_path
            {
                self.insert_workspace_member(package, Some(version), Some(root_path));
            } else {
                self.pending_workspace_packages.push(package);
            }
        }
    }

    fn resolve_pending_workspace_packages(&mut self) {
        let pending = std::mem::take(&mut self.pending_workspace_packages);
        for package in pending {
            let (version, root_path) = self
                .nearest_workspace_package(&package.abs_path)
                .map_or((None, None), |(version, root_path)| {
                    (Some(version), Some(root_path))
                });
            self.insert_workspace_member(package, version, root_path);
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

        if cargo_toml.get("workspace").is_none() {
            self.discover_workspace_dependency_aliases_for_member(path, relative_path)
                .await;
        }

        // Collect workspace dependencies for this file — the same
        // `dep_names` list feeds every branch below (workspace /
        // inherits-workspace-version / plain-package).
        let workspace_aliases = self.nearest_workspace_dependency_aliases(path);
        let dep_names: Vec<String> = workspace_dep_names(&cargo_toml)
            .into_iter()
            .map(|dependency_name| {
                workspace_aliases
                    .and_then(|aliases| aliases.get(dependency_name))
                    .map_or_else(|| dependency_name.to_string(), Clone::clone)
            })
            .collect();

        // if workspace
        if cargo_toml.get("workspace").is_some() {
            let path_key = path.to_path_buf();

            self.workspace_dependency_aliases
                .insert(path_key.clone(), workspace_dependency_aliases(&cargo_toml));

            // Read [workspace.package].version if present
            let ws_pkg_version = workspace_package_str(&cargo_toml, "version");
            if let Some(version) = ws_pkg_version {
                self.workspace_package_versions
                    .insert(path_key.clone(), version);
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
            let mut project = Project::Workspace(Box::new(RustWorkspace::new(
                name,
                version,
                path_key.clone(),
                relative_path.to_path_buf(),
            )));
            for dep_name in &dep_names {
                project.add_dependency(dep_name);
            }
            self.projects.insert(path_key.clone(), project);

            // Resolve only members contained by this workspace root.
            self.resolve_pending_workspace_packages_for_root(&path_key);
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
                let package = PendingWorkspacePackage {
                    name,
                    abs_path: path_key,
                    relative_path: relative_path_key,
                    dependencies: dep_names,
                };
                if let Some((version, root_path)) =
                    self.nearest_workspace_package(&package.abs_path)
                {
                    self.insert_workspace_member(package, Some(version), Some(root_path));
                } else {
                    self.pending_workspace_packages.push(package);
                }
            } else {
                let version = package_str(&cargo_toml, "version");
                let mut project = Project::Package(Box::new(RustPackage::new(
                    name,
                    version,
                    path_key.clone(),
                    relative_path_key,
                )));
                for dep_name in &dep_names {
                    project.add_dependency(dep_name);
                }
                self.projects.insert(path_key, project);
            }
        }
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        let unresolved_packages = self
            .pending_workspace_packages
            .iter()
            .map(|package| (package.abs_path.clone(), package.relative_path.clone()))
            .collect::<Vec<_>>();

        // Roots can be omitted by ignore patterns, so discover the nearest root
        // independently for every unresolved member.
        for (abs_path, relative_path) in unresolved_packages {
            let git_root = abs_path
                .ancestors()
                .nth(relative_path.components().count())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| abs_path.clone());

            for parent in abs_path.ancestors().skip(2) {
                if !parent.starts_with(&git_root) {
                    break;
                }
                let candidate = parent.join("Cargo.toml");
                if self.workspace_package_versions.contains_key(&candidate) {
                    break;
                }
                if is_regular_file(&candidate).await
                    && let Ok(content) = read_to_string(&candidate).await
                    && let Ok(parsed) = content.parse::<toml_edit::DocumentMut>()
                    && let Some(workspace_version) = workspace_package_str(&parsed, "version")
                {
                    let root_path = candidate;
                    self.workspace_package_versions
                        .insert(root_path.clone(), workspace_version.clone());
                    self.workspace_dependency_aliases
                        .insert(root_path.clone(), workspace_dependency_aliases(&parsed));

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
                        ws_pkg_version.or(Some(workspace_version)),
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
    use changepacks_core::{ChangePackResultLog, Project, UpdateType};
    use rstest::rstest;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;

    fn write_inherited_version_workspace(
        root: &Path,
        package_name: &str,
        version: &str,
    ) -> (PathBuf, PathBuf) {
        fs::create_dir_all(root).unwrap();
        let workspace_toml = root.join("Cargo.toml");
        fs::write(
            &workspace_toml,
            format!(
                r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "{version}"
"#
            ),
        )
        .unwrap();

        let package_dir = root.join("crates").join(package_name);
        fs::create_dir_all(&package_dir).unwrap();
        let package_toml = package_dir.join("Cargo.toml");
        fs::write(
            &package_toml,
            format!(
                r#"[package]
name = "{package_name}"
version.workspace = true
"#
            ),
        )
        .unwrap();

        (workspace_toml, package_toml)
    }

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
    async fn test_rust_project_finder_resolves_inline_and_target_table_path_aliases() {
        let temp_dir = TempDir::new().unwrap();
        let core_dir = temp_dir.path().join("crates/core");
        let target_core_dir = temp_dir.path().join("crates/target-core");
        let app_dir = temp_dir.path().join("crates/app");
        fs::create_dir_all(&core_dir).unwrap();
        fs::create_dir_all(&target_core_dir).unwrap();
        fs::create_dir_all(&app_dir).unwrap();

        let core_toml = core_dir.join("Cargo.toml");
        let target_core_toml = target_core_dir.join("Cargo.toml");
        let app_toml = app_dir.join("Cargo.toml");
        fs::write(
            &core_toml,
            "[package]\nname = \"core\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::write(
            &target_core_toml,
            "[package]\nname = \"target-core\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::write(
            &app_toml,
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
renamed-core = { package = "core", path = "../core", version = "1.0.0" }

[target.'cfg(unix)'.dependencies.renamed-target-core]
package = "target-core"
path = "../target-core"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&app_toml, &PathBuf::from("crates/app/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&core_toml, &PathBuf::from("crates/core/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(
                &target_core_toml,
                &PathBuf::from("crates/target-core/Cargo.toml"),
            )
            .await
            .unwrap();

        let app = finder
            .projects()
            .into_iter()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert_eq!(app.dependencies().len(), 2);
        assert!(app.dependencies().contains("core"));
        assert!(app.dependencies().contains("target-core"));
        assert!(!app.dependencies().contains("renamed-core"));
        assert!(!app.dependencies().contains("renamed-target-core"));

        let projects = finder.projects();
        let sorted = changepacks_utils::sort_by_dependencies(projects.clone())
            .expect("fixture graph is a DAG");
        let app_index = sorted
            .iter()
            .position(|project| project.name() == Some("app"))
            .unwrap();
        assert!(
            sorted
                .iter()
                .position(|project| project.name() == Some("core"))
                .unwrap()
                < app_index
        );
        assert!(
            sorted
                .iter()
                .position(|project| project.name() == Some("target-core"))
                .unwrap()
                < app_index
        );

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core/Cargo.toml"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );
        changepacks_utils::apply_reverse_dependencies(&mut update_map, &projects, temp_dir.path());
        assert_eq!(
            update_map[&PathBuf::from("crates/app/Cargo.toml")].0,
            UpdateType::Patch
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_resolves_workspace_inherited_alias_from_root_definition() {
        let temp_dir = TempDir::new().unwrap();
        let core_dir = temp_dir.path().join("crates/core");
        let app_dir = temp_dir.path().join("crates/app");
        fs::create_dir_all(&core_dir).unwrap();
        fs::create_dir_all(&app_dir).unwrap();

        let workspace_toml = temp_dir.path().join("Cargo.toml");
        let core_toml = core_dir.join("Cargo.toml");
        let app_toml = app_dir.join("Cargo.toml");
        fs::write(
            &workspace_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
renamed-core = { package = "core", path = "crates/core" }
"#,
        )
        .unwrap();
        fs::write(
            &core_toml,
            "[package]\nname = \"core\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::write(
            &app_toml,
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
renamed-core = { workspace = true }
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&app_toml, Path::new("crates/app/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&workspace_toml, Path::new("Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(&core_toml, Path::new("crates/core/Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        let app = projects
            .iter()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert!(app.dependencies().contains("core"));
        assert!(!app.dependencies().contains("renamed-core"));

        let sorted = changepacks_utils::sort_by_dependencies(projects.clone())
            .expect("fixture graph is a DAG");
        let core_index = sorted
            .iter()
            .position(|project| project.name() == Some("core"))
            .unwrap();
        let app_index = sorted
            .iter()
            .position(|project| project.name() == Some("app"))
            .unwrap();
        assert!(core_index < app_index);

        let mut update_map = HashMap::new();
        update_map.insert(
            PathBuf::from("crates/core/Cargo.toml"),
            (
                UpdateType::Minor,
                vec![ChangePackResultLog::new(
                    UpdateType::Minor,
                    "Update core".to_string(),
                )],
            ),
        );
        changepacks_utils::apply_reverse_dependencies(&mut update_map, &projects, temp_dir.path());
        assert_eq!(
            update_map[&PathBuf::from("crates/app/Cargo.toml")].0,
            UpdateType::Patch
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_project_finder_does_not_resolve_alias_above_repository_boundary() {
        let temp_dir = TempDir::new().unwrap();
        let repo_dir = temp_dir.path().join("repo");
        fs::create_dir_all(&repo_dir).unwrap();

        fs::write(
            temp_dir.path().join("Cargo.toml"),
            r#"[workspace]
members = ["repo"]

[workspace.dependencies]
renamed-core = { package = "core", path = "outside-core" }
"#,
        )
        .unwrap();
        let member_toml = repo_dir.join("Cargo.toml");
        fs::write(
            &member_toml,
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
renamed-core = { workspace = true }
"#,
        )
        .unwrap();

        let mut finder = RustProjectFinder::new();
        finder
            .visit(&member_toml, Path::new("Cargo.toml"))
            .await
            .unwrap();

        let projects = finder.projects();
        let app = projects
            .iter()
            .find(|project| project.name() == Some("app"))
            .unwrap();
        assert!(app.dependencies().contains("renamed-core"));
        assert!(!app.dependencies().contains("core"));

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
    async fn test_rust_project_finder_isolates_sibling_workspaces_during_interleaved_visits() {
        // Given: sibling workspaces with distinct inherited versions
        let temp_dir = TempDir::new().unwrap();
        let (alpha_workspace, alpha_package) = write_inherited_version_workspace(
            &temp_dir.path().join("alpha"),
            "alpha-package",
            "1.2.3",
        );
        let (beta_workspace, beta_package) = write_inherited_version_workspace(
            &temp_dir.path().join("beta"),
            "beta-package",
            "4.5.6",
        );

        // When: member and root visits are interleaved across the workspaces
        let mut finder = RustProjectFinder::new();
        finder
            .visit(
                &alpha_package,
                Path::new("alpha/crates/alpha-package/Cargo.toml"),
            )
            .await
            .unwrap();
        finder
            .visit(&beta_workspace, Path::new("beta/Cargo.toml"))
            .await
            .unwrap();
        finder
            .visit(
                &beta_package,
                Path::new("beta/crates/beta-package/Cargo.toml"),
            )
            .await
            .unwrap();
        finder
            .visit(&alpha_workspace, Path::new("alpha/Cargo.toml"))
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        // Then: each member inherits only from its containing workspace
        let projects = finder.projects();
        for (name, version, workspace_root) in [
            ("alpha-package", "1.2.3", &alpha_workspace),
            ("beta-package", "4.5.6", &beta_workspace),
        ] {
            let package = projects
                .iter()
                .copied()
                .find(|project| project.name() == Some(name))
                .unwrap();
            assert_eq!(package.version(), Some(version));
            match package {
                Project::Package(package) => {
                    assert_eq!(
                        package.workspace_root_path(),
                        Some(workspace_root.as_path())
                    );
                }
                Project::Workspace(_) => panic!("expected package"),
            }
        }
    }

    #[tokio::test]
    async fn test_rust_project_finder_finalize_discovers_each_unvisited_sibling_workspace() {
        // Given: inherited-version members in sibling workspaces whose roots are ignored
        let temp_dir = TempDir::new().unwrap();
        let (alpha_workspace, alpha_package) = write_inherited_version_workspace(
            &temp_dir.path().join("alpha"),
            "alpha-package",
            "1.2.3",
        );
        let (beta_workspace, beta_package) = write_inherited_version_workspace(
            &temp_dir.path().join("beta"),
            "beta-package",
            "4.5.6",
        );

        // When: only the members are visited before finalization
        let mut finder = RustProjectFinder::new();
        finder
            .visit(
                &alpha_package,
                Path::new("alpha/crates/alpha-package/Cargo.toml"),
            )
            .await
            .unwrap();
        finder
            .visit(
                &beta_package,
                Path::new("beta/crates/beta-package/Cargo.toml"),
            )
            .await
            .unwrap();
        finder.finalize().await.unwrap();

        // Then: finalization discovers and applies each member's own workspace root
        let projects = finder.projects();
        for (name, version, workspace_root) in [
            ("alpha-package", "1.2.3", &alpha_workspace),
            ("beta-package", "4.5.6", &beta_workspace),
        ] {
            let package = projects
                .iter()
                .copied()
                .find(|project| project.name() == Some(name))
                .unwrap();
            assert_eq!(package.version(), Some(version));
            match package {
                Project::Package(package) => {
                    assert_eq!(
                        package.workspace_root_path(),
                        Some(workspace_root.as_path())
                    );
                }
                Project::Workspace(_) => panic!("expected package"),
            }
        }
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
