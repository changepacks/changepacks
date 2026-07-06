use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug)]
pub struct PythonPackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl PythonPackage {
    // Standard package/workspace constructor.
    changepacks_core::impl_default_new!();
}

#[async_trait]
impl Package for PythonPackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // `update_version` shares its body with `PythonWorkspace` — only the
    // `ensure_project_table` bool passed to `write_pyproject_version`
    // differs (`false` here, `true` in `PythonWorkspace`). Consolidated
    // via the shared `update_version_from_fields` helper in
    // `crates/python/src/lib.rs` so the "reserve `0.0.0`" fallback and
    // `next_version` computation live in ONE place. See the helper's
    // doc comment for why a `macro_rules!` producing `async fn` is
    // incompatible with `#[async_trait]` (E0195 lifetime mismatch).
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::update_version_from_fields(&mut self.version, &self.path, update_type, false).await
    }

    // Fixed language accessor.
    changepacks_core::impl_language!(Language::Python);

    // Const publish defaults; `publishDryRun` can override this preview command.
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

    fn assert_python_package_defaults(package: &PythonPackage) {
        assert_eq!(package.name(), Some("test-package"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), PathBuf::from("/test/pyproject.toml"));
        assert_eq!(
            package.relative_path(),
            PathBuf::from("test/pyproject.toml")
        );
        assert_eq!(package.language(), Language::Python);
        assert!(!package.is_changed());
        assert_eq!(package.default_publish_command(), "uv publish");
        assert_eq!(
            package.default_dry_run_publish_command().as_deref(),
            Some("uv publish --dry-run")
        );
    }

    #[tokio::test]
    async fn test_python_package_new() {
        let package = PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert_python_package_defaults(&package);
    }

    #[tokio::test]
    async fn test_python_package_set_changed() {
        let mut package = PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert!(!package.is_changed());
        package.set_changed(true);
        assert!(package.is_changed());
        package.set_changed(false);
        assert!(!package.is_changed());
    }

    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_python_package_update_version(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
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

        let mut package = PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        package.update_version(update_type).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains(&format!("version = \"{expected}\"")));

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

        let mut package = PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.2.3".to_string()),
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

    #[test]
    fn test_python_package_dependencies() {
        let mut package = PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        // Initially empty
        assert!(package.dependencies().is_empty());

        // Add dependencies
        package.add_dependency("requests");
        package.add_dependency("core");

        let deps = package.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("requests"));
        assert!(deps.contains("core"));

        // Adding duplicate should not increase count
        package.add_dependency("requests");
        assert_eq!(package.dependencies().len(), 2);
    }

    #[test]
    fn test_set_name() {
        let mut package = PythonPackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("pyproject.toml"),
        );
        assert_eq!(package.name(), None);
        package.set_name("my-project".to_string());
        assert_eq!(package.name(), Some("my-project"));
    }
}
