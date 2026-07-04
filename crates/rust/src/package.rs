use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::next_version;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::write_cargo_package_version;

#[derive(Debug)]
pub struct RustPackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
    dependencies: HashSet<String>,
    workspace_version_inherited: bool,
    workspace_root: Option<PathBuf>,
}

impl RustPackage {
    #[must_use]
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
            dependencies: HashSet::new(),
            workspace_version_inherited: false,
            workspace_root: None,
        }
    }

    #[must_use]
    pub fn new_with_workspace_version(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
            dependencies: HashSet::new(),
            workspace_version_inherited: true,
            workspace_root,
        }
    }
}

#[async_trait]
impl Package for RustPackage {
    fn relative_path(&self) -> &Path {
        &self.relative_path
    }
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        // Members that inherit `version.workspace = true` from the workspace
        // root MUST NOT rewrite their own `[package].version` here —
        // `write_cargo_package_version` would clobber the inheritance
        // marker with a plain string, silently breaking the workspace-wide
        // version link. The bump for those members is owned by the
        // workspace root's `RustWorkspace::update_version`, which drives
        // `[workspace.package].version` (and fans out into workspace
        // dependencies). For inheriting members, the correct action here
        // is a no-op.
        if self.workspace_version_inherited {
            return Ok(());
        }
        let current_version = self.version.as_deref().unwrap_or("0.0.0");
        let new_version = next_version(current_version, update_type)?;
        write_cargo_package_version(&self.path, &new_version).await?;
        self.version = Some(new_version);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn default_publish_command(&self) -> String {
        crate::PUBLISH_COMMAND.to_string()
    }

    fn default_dry_run_publish_command(&self) -> Option<String> {
        Some(crate::DRY_RUN_PUBLISH_COMMAND.to_string())
    }

    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }

    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
    }

    fn inherits_workspace_version(&self) -> bool {
        self.workspace_version_inherited
    }

    fn workspace_root_path(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
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
    async fn test_rust_package_new() {
        let package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(package.name(), Some("test-package"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), PathBuf::from("/test/Cargo.toml"));
        assert_eq!(package.relative_path(), PathBuf::from("test/Cargo.toml"));
        assert_eq!(package.language(), Language::Rust);
        assert!(!package.is_changed());
        assert_eq!(package.default_publish_command(), "cargo publish");
        assert_eq!(
            package.default_dry_run_publish_command().as_deref(),
            Some("cargo publish --dry-run")
        );
    }

    #[tokio::test]
    async fn test_rust_package_set_changed() {
        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert!(!package.is_changed());
        package.set_changed(true);
        assert!(package.is_changed());
        package.set_changed(false);
        assert!(!package.is_changed());
    }

    #[tokio::test]
    async fn test_rust_package_update_version_patch() {
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

        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"1.0.1\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_package_update_version_minor() {
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

        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        package.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"1.1.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_package_update_version_major() {
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

        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        package.update_version(UpdateType::Major).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"2.0.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_package_update_version_preserves_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.2.3"
edition = "2021"

[dependencies]
tokio = "1.0"
"#,
        )
        .unwrap();

        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.2.3".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"1.2.4\""));
        assert!(content.contains("name = \"test-package\""));
        assert!(content.contains("edition = \"2021\""));
        assert!(content.contains("[dependencies]"));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_rust_package_dependencies() {
        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        // Initially empty
        assert!(package.dependencies().is_empty());

        // Add dependencies
        package.add_dependency("core");
        package.add_dependency("utils");

        let deps = package.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("core"));
        assert!(deps.contains("utils"));

        // Adding duplicate should not increase count
        package.add_dependency("core");
        assert_eq!(package.dependencies().len(), 2);
    }

    #[test]
    fn test_rust_package_inherits_workspace_version() {
        let package = RustPackage::new(
            Some("test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );
        assert!(!package.inherits_workspace_version());
        assert!(package.workspace_root_path().is_none());
    }

    #[test]
    fn test_rust_package_with_workspace_version() {
        let package = RustPackage::new_with_workspace_version(
            Some("test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/crates/foo/Cargo.toml"),
            PathBuf::from("crates/foo/Cargo.toml"),
            Some(PathBuf::from("/test/Cargo.toml")),
        );
        assert!(package.inherits_workspace_version());
        assert_eq!(
            package.workspace_root_path(),
            Some(Path::new("/test/Cargo.toml"))
        );
    }

    #[test]
    fn test_set_name() {
        let mut package = RustPackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("Cargo.toml"),
        );
        assert_eq!(package.name(), None);
        package.set_name("my-project".to_string());
        assert_eq!(package.name(), Some("my-project"));
    }

    #[tokio::test]
    async fn test_rust_package_update_version_preserves_workspace_inheritance() {
        // Regression: `RustPackage::update_version` used to unconditionally
        // rewrite `[package].version = "..."`. On a member that inherits its
        // version from the workspace root (`version.workspace = true`), that
        // silently clobbered the inheritance marker with a plain string —
        // permanently detaching the member from the workspace-wide bump.
        // The correct action for an inheriting member is a no-op here; the
        // workspace root owns the bump.
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        let original_content = r#"[package]
name = "test-inherited"
version.workspace = true
edition = "2024"

[dependencies]
"#;
        fs::write(&cargo_toml, original_content).unwrap();

        let mut package = RustPackage::new_with_workspace_version(
            Some("test-inherited".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("crates/test-inherited/Cargo.toml"),
            Some(PathBuf::from("/test/Cargo.toml")),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        // The on-disk manifest still declares the workspace-inherited
        // marker — no rewritten `version = "X.Y.Z"` line.
        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(
            content.contains("version.workspace = true"),
            "inheritance marker was clobbered: {content}"
        );
        assert!(
            !content.contains("version = \""),
            "member Cargo.toml gained a hardcoded version string: {content}"
        );
        // `self.version` remains the originally-observed inherited value —
        // NOT the bumped `1.0.1` — because the workspace root owns the bump
        // and will reflect it back through `new_with_workspace_version`.
        assert_eq!(package.version(), Some("1.0.0"));
    }
}
