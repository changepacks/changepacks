use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Package, Project, ProjectFinder};
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

#[derive(Debug)]
pub struct RustProjectFinder {
    projects: HashMap<PathBuf, Project>,
    project_files: Vec<&'static str>,
    workspace_package_version: Option<String>,
    workspace_root_path: Option<PathBuf>,
    pending_workspace_packages: Vec<PendingWorkspacePackage>,
}

impl Default for RustProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl RustProjectFinder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec!["Cargo.toml"],
            workspace_package_version: None,
            workspace_root_path: None,
            pending_workspace_packages: Vec::new(),
        }
    }
}

#[async_trait]
impl ProjectFinder for RustProjectFinder {
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
                    .context(format!("File name not found - {}", path.display()))?
                    .to_str()
                    .context(format!("File name not found - {}", path.display()))?,
            )
        {
            if self.projects.contains_key(path) {
                return Ok(());
            }
            // read Cargo.toml
            let cargo_toml = read_to_string(path).await?;
            let cargo_toml: toml::Value = toml::from_str(&cargo_toml)?;

            // Collect workspace dependencies for this file
            let mut dep_names = Vec::new();
            if let Some(deps) = cargo_toml.get("dependencies").and_then(|d| d.as_table()) {
                for (dep_name, value) in deps {
                    if let Some(dep) = value.as_table()
                        && let Some(workspace) = dep.get("workspace")
                        && workspace.as_bool().unwrap_or(false)
                    {
                        dep_names.push(dep_name.clone());
                    }
                }
            }

            // if workspace
            if cargo_toml.get("workspace").is_some() {
                // Read [workspace.package].version if present
                let ws_pkg_version = cargo_toml
                    .get("workspace")
                    .and_then(|w| w.get("package"))
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if ws_pkg_version.is_some() {
                    self.workspace_package_version = ws_pkg_version;
                    self.workspace_root_path = Some(path.to_path_buf());
                }

                let version = cargo_toml
                    .get("package")
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string);
                let name = cargo_toml
                    .get("package")
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string);
                let mut project = Project::Workspace(Box::new(RustWorkspace::new(
                    name,
                    version,
                    path.to_path_buf(),
                    relative_path.to_path_buf(),
                )));
                for dep_name in &dep_names {
                    project.add_dependency(dep_name);
                }
                self.projects.insert(path.to_path_buf(), project);

                // Resolve any pending packages that were visited before this workspace
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
            } else {
                // Check if version.workspace = true
                let inherits_workspace = cargo_toml
                    .get("package")
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_table())
                    .and_then(|t| t.get("workspace"))
                    .and_then(|w| w.as_bool())
                    .unwrap_or(false);

                let name = cargo_toml["package"]["name"]
                    .as_str()
                    .map(std::string::ToString::to_string);

                if inherits_workspace {
                    if self.workspace_package_version.is_some() {
                        // Workspace already visited — resolve immediately
                        let mut pkg = RustPackage::new_with_workspace_version(
                            name,
                            self.workspace_package_version.clone(),
                            path.to_path_buf(),
                            relative_path.to_path_buf(),
                            self.workspace_root_path.clone(),
                        );
                        for dep_name in &dep_names {
                            pkg.add_dependency(dep_name);
                        }
                        self.projects
                            .insert(path.to_path_buf(), Project::Package(Box::new(pkg)));
                    } else {
                        // Workspace not yet visited — defer
                        self.pending_workspace_packages
                            .push(PendingWorkspacePackage {
                                name,
                                abs_path: path.to_path_buf(),
                                relative_path: relative_path.to_path_buf(),
                                dependencies: dep_names,
                            });
                    }
                } else {
                    let version = cargo_toml["package"]["version"]
                        .as_str()
                        .map(std::string::ToString::to_string);
                    let mut project = Project::Package(Box::new(RustPackage::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    )));
                    for dep_name in &dep_names {
                        project.add_dependency(dep_name);
                    }
                    self.projects.insert(path.to_path_buf(), project);
                }
            };
        }
        Ok(())
    }

    async fn finalize(&mut self) -> Result<()> {
        // If workspace root was never visited (e.g. excluded by ignore patterns),
        // walk up from the first pending package to find and read it
        if self.workspace_package_version.is_none()
            && !self.pending_workspace_packages.is_empty()
            && let Some(first_pkg) = self.pending_workspace_packages.first()
        {
            // Derive git root from the first pending package's absolute/relative paths
            // e.g. abs=<repo>/crates/foo/Cargo.toml, rel=crates/foo/Cargo.toml → git_root=<repo>
            let rel_component_count = first_pkg.relative_path.components().count();
            let mut git_root = first_pkg.abs_path.clone();
            for _ in 0..rel_component_count {
                git_root.pop();
            }

            let mut dir = first_pkg.abs_path.parent().and_then(Path::parent);
            while let Some(parent) = dir {
                let candidate = parent.join("Cargo.toml");
                if candidate.is_file()
                    && let Ok(content) = read_to_string(&candidate).await
                    && let Ok(parsed) = toml::from_str::<toml::Value>(&content)
                    && let Some(version) = parsed
                        .get("workspace")
                        .and_then(|w| w.get("package"))
                        .and_then(|p| p.get("version"))
                        .and_then(|v| v.as_str())
                {
                    self.workspace_package_version = Some(version.to_string());
                    self.workspace_root_path = Some(candidate.clone());

                    // Insert synthetic workspace project so apply_updates() can find it
                    let ws_name = parsed
                        .get("package")
                        .and_then(|p| p.get("name"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let ws_pkg_version = parsed
                        .get("package")
                        .and_then(|p| p.get("version"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let ws_relative_path = candidate
                        .strip_prefix(&git_root)
                        .unwrap_or(Path::new("Cargo.toml"))
                        .to_path_buf();

                    let workspace = RustWorkspace::new(
                        ws_name,
                        // For virtual workspaces (no [package]), use [workspace.package].version
                        ws_pkg_version.or_else(|| self.workspace_package_version.clone()),
                        candidate,
                        ws_relative_path,
                    );
                    self.projects.insert(
                        self.workspace_root_path.clone().unwrap(),
                        Project::Workspace(Box::new(workspace)),
                    );
                    break;
                }
                dir = parent.parent();
            }
        }

        for pending in self.pending_workspace_packages.drain(..) {
            let mut pkg = RustPackage::new_with_workspace_version(
                pending.name,
                self.workspace_package_version.clone(),
                pending.abs_path.clone(),
                pending.relative_path,
                self.workspace_root_path.clone(),
            );
            for dep in &pending.dependencies {
                pkg.add_dependency(dep);
            }
            self.projects
                .insert(pending.abs_path, Project::Package(Box::new(pkg)));
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
    fn test_rust_project_finder_new() {
        let finder = RustProjectFinder::new();
        assert_eq!(finder.project_files(), &["Cargo.toml"]);
        assert_eq!(finder.projects().len(), 0);
    }

    #[test]
    fn test_rust_project_finder_default() {
        let finder = RustProjectFinder::default();
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
}
