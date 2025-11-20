use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::{detect_indent, next_version};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};

#[derive(Debug)]
pub struct NodePackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
}

impl NodePackage {
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
        }
    }
}

#[async_trait]
impl Package for NodePackage {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let current_version = self.version.as_deref().unwrap_or("0.0.0");
        let new_version = next_version(current_version, update_type)?;

        let package_json_raw = read_to_string(&self.path).await?;
        let indent = detect_indent(&package_json_raw);
        let mut package_json: serde_json::Value = serde_json::from_str(&package_json_raw)?;
        package_json["version"] = serde_json::Value::String(new_version.clone());
        let ind = &b" ".repeat(indent);
        let formatter = serde_json::ser::PrettyFormatter::with_indent(ind);
        let writer = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(writer, formatter);
        package_json.serialize(&mut ser)?;
        write(
            &self.path,
            format!(
                "{}{}",
                String::from_utf8(ser.into_inner())?.to_string().trim_end(),
                if package_json_raw.ends_with("\n") {
                    "\n"
                } else {
                    ""
                }
            ),
        )
        .await?;
        self.version = Some(new_version);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Node
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }
    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn default_publish_command(&self) -> &'static str {
        "npm publish"
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
    async fn test_node_package_new() {
        let package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
        );

        assert_eq!(package.name(), Some("test-package"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), PathBuf::from("/test/package.json"));
        assert_eq!(package.relative_path(), PathBuf::from("test/package.json"));
        assert_eq!(package.language(), Language::Node);
        assert_eq!(package.is_changed(), false);
        assert_eq!(package.default_publish_command(), "npm publish");
    }

    #[tokio::test]
    async fn test_node_package_set_changed() {
        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
        );

        assert_eq!(package.is_changed(), false);
        package.set_changed(true);
        assert_eq!(package.is_changed(), true);
        package.set_changed(false);
        assert_eq!(package.is_changed(), false);
    }

    #[tokio::test]
    async fn test_node_package_update_version_patch() {
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

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.0.1""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_package_update_version_minor() {
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

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.1.0""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_package_update_version_major() {
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

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Major).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "2.0.0""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_package_update_version_preserves_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package",
  "version": "1.2.3",
  "description": "A test package",
  "dependencies": {
    "express": "^4.18.0"
  }
}
"#,
        )
        .unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.2.3".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.2.4""#));
        assert!(content.contains(r#""name": "test-package""#));
        assert!(content.contains(r#""description": "A test package""#));
        assert!(content.contains(r#""dependencies""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_package_update_version_preserves_newline() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{"name":"test-package","version":"1.0.0"}
"#,
        )
        .unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.ends_with('\n'));
        assert!(content.contains(r#""version": "1.0.1""#));

        temp_dir.close().unwrap();
    }
}
