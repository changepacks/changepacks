use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use changepacks_utils::next_version;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};
use toml_edit::DocumentMut;

#[derive(Debug)]
pub struct PythonWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl PythonWorkspace {
    #[must_use]
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
impl Workspace for PythonWorkspace {
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

        let pyproject_toml_raw = read_to_string(&self.path).await?;
        let mut pyproject_toml: DocumentMut = pyproject_toml_raw.parse::<DocumentMut>()?;
        if pyproject_toml.get("project").is_none() {
            pyproject_toml["project"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        pyproject_toml["project"]["version"] = next_version.clone().into();
        write(
            &self.path,
            format!(
                "{}{}",
                pyproject_toml.to_string().trim_end(),
                if pyproject_toml_raw.ends_with('\n') {
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
        Language::Python
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

    fn set_name(&mut self, name: String) {
        self.name = Some(name);
    }

    fn default_publish_command(&self) -> String {
        "uv publish".to_string()
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
    async fn test_python_workspace_new() {
        let workspace = PythonWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert_eq!(workspace.name(), Some("test-workspace"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), PathBuf::from("/test/pyproject.toml"));
        assert_eq!(
            workspace.relative_path(),
            PathBuf::from("test/pyproject.toml")
        );
        assert_eq!(workspace.language(), Language::Python);
        assert!(!workspace.is_changed());
        assert_eq!(workspace.default_publish_command(), "uv publish");
    }

    #[tokio::test]
    async fn test_python_workspace_new_without_name_and_version() {
        let workspace = PythonWorkspace::new(
            None,
            None,
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
    }

    #[tokio::test]
    async fn test_python_workspace_set_changed() {
        let mut workspace = PythonWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert!(!workspace.is_changed());
        workspace.set_changed(true);
        assert!(workspace.is_changed());
        workspace.set_changed(false);
        assert!(!workspace.is_changed());
    }

    #[tokio::test]
    async fn test_python_workspace_update_version_with_existing_project() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]

[project]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut workspace = PythonWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("version = \"1.0.1\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_workspace_update_version_without_project_section() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]
"#,
        )
        .unwrap();

        let mut workspace = PythonWorkspace::new(
            Some("test-workspace".to_string()),
            None,
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("[project]"));
        assert!(content.contains("version = \"0.0.1\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_workspace_update_version_minor() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]

[project]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut workspace = PythonWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        workspace.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("version = \"1.1.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_python_workspace_update_version_major() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[tool.uv.workspace]
members = ["packages/*"]

[project]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut workspace = PythonWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        workspace.update_version(UpdateType::Major).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("version = \"2.0.0\""));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_python_workspace_dependencies() {
        let mut workspace = PythonWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        // Initially empty
        assert!(workspace.dependencies().is_empty());

        // Add dependencies
        workspace.add_dependency("requests");
        workspace.add_dependency("core");

        let deps = workspace.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("requests"));
        assert!(deps.contains("core"));

        // Adding duplicate should not increase count
        workspace.add_dependency("requests");
        assert_eq!(workspace.dependencies().len(), 2);
    }

    #[test]
    fn test_set_name() {
        let mut workspace = PythonWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("pyproject.toml"),
        );
        assert_eq!(workspace.name(), None);
        workspace.set_name("my-project".to_string());
        assert_eq!(workspace.name(), Some("my-project"));
    }
}
