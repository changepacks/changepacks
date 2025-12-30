use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType, Workspace};
use changepacks_utils::{next_version, split_version};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};
use toml_edit::DocumentMut;

#[derive(Debug)]
pub struct RustWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl RustWorkspace {
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self {
            path,
            relative_path,
            name,
            version,
            is_changed: false,
            dependencies: HashSet::new(),
        }
    }
}

#[async_trait]
impl Workspace for RustWorkspace {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(
            self.version.as_ref().unwrap_or(&String::from("0.0.0")),
            update_type,
        )?;

        let cargo_toml_raw = read_to_string(&self.path).await?;
        let mut cargo_toml: DocumentMut = cargo_toml_raw.parse::<DocumentMut>()?;
        if cargo_toml.get("package").is_none() {
            cargo_toml["package"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        cargo_toml["package"]["version"] = next_version.clone().into();
        if cargo_toml
            .get("package")
            .and_then(|p| p.get("name"))
            .is_none()
        {
            // insert package.name with version, cargo rules
            cargo_toml["package"]["name"] = self.name.clone().unwrap_or("_".to_string()).into();
        }

        write(
            &self.path,
            format!(
                "{}{}",
                cargo_toml.to_string().trim_end(),
                if cargo_toml_raw.ends_with("\n") {
                    "\n"
                } else {
                    ""
                }
            ),
        )
        .await?;
        self.version = Some(next_version);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    fn default_publish_command(&self) -> String {
        "cargo publish".to_string()
    }

    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }

    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
    }

    async fn update_workspace_dependencies(&self, packages: &[&dyn Package]) -> Result<()> {
        let cargo_toml_raw = read_to_string(&self.path).await?;
        let mut cargo_toml: DocumentMut = cargo_toml_raw.parse::<DocumentMut>()?;

        // check has workspace.dependencies section
        if cargo_toml.get("workspace").is_none()
            || cargo_toml["workspace"].get("dependencies").is_none()
        {
            return Ok(());
        }
        let dependencies = cargo_toml
            .get_mut("workspace")
            .and_then(|w| w.get_mut("dependencies"))
            .and_then(|d| d.as_table_mut())
            .context("Dependencies section not found")?;

        for package in packages {
            if package.language() != Language::Rust {
                continue;
            }
            let Some(package_name) = package.name() else {
                continue;
            };
            if dependencies.get(package_name).is_none() {
                continue;
            }

            let dep = dependencies[package_name].as_inline_table_mut();
            if let Some(dep) = dep {
                let (prefix, _) = split_version(dep["version"].as_str().unwrap_or(""))?;
                dep["version"] = format!(
                    "{}{}",
                    prefix.unwrap_or("".to_string()),
                    package.version().unwrap_or("0.0.0")
                )
                .into();
            }
        }

        write(
            &self.path,
            format!(
                "{}{}",
                cargo_toml.to_string().trim_end(),
                if cargo_toml_raw.ends_with("\n") {
                    "\n"
                } else {
                    ""
                }
            ),
        )
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::UpdateType;
    use std::fs;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    #[tokio::test]
    async fn test_rust_workspace_new() {
        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(workspace.name(), Some("test-workspace"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), PathBuf::from("/test/Cargo.toml"));
        assert_eq!(workspace.relative_path(), PathBuf::from("test/Cargo.toml"));
        assert_eq!(workspace.language(), Language::Rust);
        assert_eq!(workspace.is_changed(), false);
        assert_eq!(workspace.default_publish_command(), "cargo publish");
    }

    #[tokio::test]
    async fn test_rust_workspace_new_without_name_and_version() {
        let workspace = RustWorkspace::new(
            None,
            None,
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
    }

    #[tokio::test]
    async fn test_rust_workspace_set_changed() {
        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(workspace.is_changed(), false);
        workspace.set_changed(true);
        assert_eq!(workspace.is_changed(), true);
        workspace.set_changed(false);
        assert_eq!(workspace.is_changed(), false);
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_with_existing_package() {
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

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"1.0.1\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_without_package_section() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            None,
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("[package]"));
        assert!(content.contains("version = \"0.0.1\""));
        assert!(content.contains("name = \"test-workspace\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_without_name() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]
"#,
        )
        .unwrap();

        let mut workspace =
            RustWorkspace::new(None, None, cargo_toml.clone(), PathBuf::from("Cargo.toml"));

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("[package]"));
        assert!(content.contains("version = \"0.0.1\""));
        assert!(content.contains("name = \"_\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_minor() {
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

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"1.1.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_major() {
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

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Major).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"2.0.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_preserves_existing_name() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[package]
name = "existing-name"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("name = \"existing-name\""));
        assert!(content.contains("version = \"1.0.1\""));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_rust_workspace_dependencies() {
        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        // Initially empty
        assert!(workspace.dependencies().is_empty());

        // Add dependencies
        workspace.add_dependency("core");
        workspace.add_dependency("utils");

        let deps = workspace.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("core"));
        assert!(deps.contains("utils"));

        // Adding duplicate should not increase count
        workspace.add_dependency("core");
        assert_eq!(workspace.dependencies().len(), 2);
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
core = { version = "1.0.0", path = "crates/core" }
utils = { version = "2.0.0", path = "crates/utils" }
"#,
        )
        .unwrap();

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        // Create mock packages with updated versions
        let mut core_pkg = RustPackage::new(
            Some("core".to_string()),
            Some("1.1.0".to_string()),
            PathBuf::from("/test/crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );
        core_pkg.set_changed(true);

        let packages: Vec<&dyn Package> = vec![&core_pkg];

        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"1.1.0\""));
        // utils should remain unchanged
        assert!(content.contains("version = \"2.0.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies_no_workspace_section() {
        use crate::package::RustPackage;

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

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        let core_pkg = RustPackage::new(
            Some("core".to_string()),
            Some("1.1.0".to_string()),
            PathBuf::from("/test/crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );

        let packages: Vec<&dyn Package> = vec![&core_pkg];

        // Should complete without error even without workspace.dependencies
        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        temp_dir.close().unwrap();
    }
}
