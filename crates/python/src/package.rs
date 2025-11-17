use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::next_version;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};
use toml_edit::DocumentMut;

#[derive(Debug)]
pub struct PythonPackage {
    name: String,
    version: String,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
}

impl PythonPackage {
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
impl Package for PythonPackage {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    async fn update_version(&self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(&self.version, update_type)?;

        let pyproject_toml = read_to_string(&self.path).await?;
        let mut pyproject_toml: DocumentMut = pyproject_toml.parse::<DocumentMut>()?;
        pyproject_toml["project"]["version"] = next_version.into();
        write(&self.path, pyproject_toml.to_string()).await?;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Python
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn default_publish_command(&self) -> &'static str {
        "uv publish"
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
    async fn test_python_package_new() {
        let package = PythonPackage::new(
            "test-package".to_string(),
            "1.0.0".to_string(),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert_eq!(package.name(), "test-package");
        assert_eq!(package.version(), "1.0.0");
        assert_eq!(package.path(), PathBuf::from("/test/pyproject.toml"));
        assert_eq!(
            package.relative_path(),
            PathBuf::from("test/pyproject.toml")
        );
        assert_eq!(package.language(), Language::Python);
        assert_eq!(package.is_changed(), false);
        assert_eq!(package.default_publish_command(), "uv publish");
    }

    #[tokio::test]
    async fn test_python_package_set_changed() {
        let mut package = PythonPackage::new(
            "test-package".to_string(),
            "1.0.0".to_string(),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert_eq!(package.is_changed(), false);
        package.set_changed(true);
        assert_eq!(package.is_changed(), true);
        package.set_changed(false);
        assert_eq!(package.is_changed(), false);
    }

    #[tokio::test]
    async fn test_python_package_update_version_patch() {
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

        let package = PythonPackage::new(
            "test-package".to_string(),
            "1.0.0".to_string(),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("version = \"1.0.1\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_package_update_version_minor() {
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

        let package = PythonPackage::new(
            "test-package".to_string(),
            "1.0.0".to_string(),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        package.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("version = \"1.1.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_package_update_version_major() {
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

        let package = PythonPackage::new(
            "test-package".to_string(),
            "1.0.0".to_string(),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        package.update_version(UpdateType::Major).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("version = \"2.0.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_package_update_version_preserves_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[project]
name = "test-package"
version = "1.2.3"
description = "A test package"
requires-python = ">=3.8"

[dependencies]
requests = "2.31.0"
"#,
        )
        .unwrap();

        let package = PythonPackage::new(
            "test-package".to_string(),
            "1.2.3".to_string(),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("version = \"1.2.4\""));
        assert!(content.contains("name = \"test-package\""));
        assert!(content.contains("description = \"A test package\""));
        assert!(content.contains("[dependencies]"));

        temp_dir.close().unwrap();
    }
}
