use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use std::collections::HashSet;
use std::path::PathBuf;

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
    // Standard package/workspace constructor.
    changepacks_core::impl_default_new!();
}

#[async_trait]
impl Workspace for PythonWorkspace {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        let path = &self.path;
        changepacks_utils::bump_version_with(&mut self.version, path, update_type, async |new| {
            crate::write_pyproject_version(path, new).await
        })
        .await
    }

    // Fixed language accessor.
    changepacks_core::impl_language!(Language::Python);

    // Const publish defaults.
    changepacks_core::impl_const_publish_commands!(
        crate::PUBLISH_COMMAND,
        crate::DRY_RUN_PUBLISH_COMMAND
    );

    // Dependency set accessors.
    changepacks_core::impl_dependencies_accessors!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::UpdateType;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    fn assert_python_workspace_defaults(workspace: &PythonWorkspace) {
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
        assert_eq!(
            workspace.default_dry_run_publish_command().as_deref(),
            Some("uv publish --dry-run")
        );
    }

    #[tokio::test]
    async fn test_python_workspace_new() {
        let workspace = PythonWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert_python_workspace_defaults(&workspace);
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

    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_python_workspace_update_version_with_existing_project(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
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

        workspace.update_version(update_type).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains(&format!("version = \"{expected}\"")));

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
