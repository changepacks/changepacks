use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::next_version;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};
use toml_edit::DocumentMut;

#[derive(Debug)]
pub struct RustPackage {
    name: String,
    version: String,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
}

impl RustPackage {
    pub fn new(name: String, version: String, path: PathBuf, relative_path: PathBuf) -> Self {
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
        }
    }
}

#[async_trait]
impl Package for RustPackage {
    fn relative_path(&self) -> &Path {
        &self.relative_path
    }
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn path(&self) -> &Path {
        &self.path
    }

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(&self.version, update_type)?;

        let cargo_toml_raw = read_to_string(&self.path).await?;
        let mut cargo_toml: DocumentMut = cargo_toml_raw.parse::<DocumentMut>()?;
        cargo_toml["package"]["version"] = next_version.clone().into();
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
        self.version = next_version;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn default_publish_command(&self) -> &'static str {
        "cargo publish"
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
            "test-package".to_string(),
            "1.0.0".to_string(),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(package.name(), "test-package");
        assert_eq!(package.version(), "1.0.0");
        assert_eq!(package.path(), PathBuf::from("/test/Cargo.toml"));
        assert_eq!(package.relative_path(), PathBuf::from("test/Cargo.toml"));
        assert_eq!(package.language(), Language::Rust);
        assert_eq!(package.is_changed(), false);
        assert_eq!(package.default_publish_command(), "cargo publish");
    }

    #[tokio::test]
    async fn test_rust_package_set_changed() {
        let mut package = RustPackage::new(
            "test-package".to_string(),
            "1.0.0".to_string(),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(package.is_changed(), false);
        package.set_changed(true);
        assert_eq!(package.is_changed(), true);
        package.set_changed(false);
        assert_eq!(package.is_changed(), false);
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
            "test-package".to_string(),
            "1.0.0".to_string(),
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
            "test-package".to_string(),
            "1.0.0".to_string(),
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
            "test-package".to_string(),
            "1.0.0".to_string(),
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
            "test-package".to_string(),
            "1.2.3".to_string(),
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
}
