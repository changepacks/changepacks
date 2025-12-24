use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use changepacks_utils::{detect_indent, next_version};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};

#[derive(Debug)]
pub struct NodeWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl NodeWorkspace {
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
impl Workspace for NodeWorkspace {
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

        let package_json_raw = read_to_string(Path::new(&self.path)).await?;
        let indent = detect_indent(&package_json_raw);
        let mut package_json: serde_json::Value = serde_json::from_str(&package_json_raw)?;
        package_json["version"] = serde_json::Value::String(next_version.clone());
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
        self.version = Some(next_version);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Node
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

    fn default_publish_command(&self) -> &'static str {
        "npm publish"
    }

    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }

    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
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
    async fn test_node_workspace_new() {
        let workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
        );

        assert_eq!(workspace.name(), Some("test-workspace"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), PathBuf::from("/test/package.json"));
        assert_eq!(
            workspace.relative_path(),
            PathBuf::from("test/package.json")
        );
        assert_eq!(workspace.language(), Language::Node);
        assert_eq!(workspace.is_changed(), false);
        assert_eq!(workspace.default_publish_command(), "npm publish");
    }

    #[tokio::test]
    async fn test_node_workspace_new_without_name_and_version() {
        let workspace = NodeWorkspace::new(
            None,
            None,
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
    }

    #[tokio::test]
    async fn test_node_workspace_set_changed() {
        let mut workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
        );

        assert_eq!(workspace.is_changed(), false);
        workspace.set_changed(true);
        assert_eq!(workspace.is_changed(), true);
        workspace.set_changed(false);
        assert_eq!(workspace.is_changed(), false);
    }

    #[tokio::test]
    async fn test_node_workspace_update_version_with_existing_version() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-workspace",
  "version": "1.0.0",
  "workspaces": ["packages/*"]
}
"#,
        )
        .unwrap();

        let mut workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.0.1""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_workspace_update_version_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-workspace",
  "workspaces": ["packages/*"]
}
"#,
        )
        .unwrap();

        let mut workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            None,
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "0.0.1""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_workspace_update_version_minor() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-workspace",
  "version": "1.0.0",
  "workspaces": ["packages/*"]
}
"#,
        )
        .unwrap();

        let mut workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        workspace.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.1.0""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_workspace_update_version_major() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-workspace",
  "version": "1.0.0",
  "workspaces": ["packages/*"]
}
"#,
        )
        .unwrap();

        let mut workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        workspace.update_version(UpdateType::Major).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "2.0.0""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_node_workspace_update_version_preserves_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-workspace",
  "version": "1.0.0",
  "workspaces": ["packages/*"],
  "scripts": {
    "test": "jest"
  }
}
"#,
        )
        .unwrap();

        let mut workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(r#""version": "1.0.1""#));
        assert!(content.contains(r#""name": "test-workspace""#));
        assert!(content.contains(r#""workspaces""#));
        assert!(content.contains(r#""scripts""#));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_node_workspace_dependencies() {
        let mut workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
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
}
