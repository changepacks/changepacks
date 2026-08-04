use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};

// Seven-field discovered-project declaration plus `new` / `new_discovered`,
// shared verbatim with the other four identical language types.
changepacks_core::declare_discovered_project!(pub struct PythonPackage);

#[async_trait]
impl Package for PythonPackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // Publishability flag accessor.
    changepacks_core::impl_publishable_by_default!();

    // Body shared with `PythonWorkspace::update_version` via the crate-local
    // helper; the signature stays hand-written because `async_trait` forbids
    // generating it from a macro (see `bump_pyproject_version` in `lib.rs`).
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::bump_pyproject_version(&mut self.version, &self.path, update_type).await
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
    use std::path::PathBuf;
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

    #[test]
    fn test_python_package_new() {
        let package = PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        );

        assert_python_package_defaults(&package);
        assert!(package.is_publishable_by_default());
    }

    #[rstest]
    #[case(true)]
    #[case(false)]
    fn test_python_package_discovered_publishability(#[case] expected: bool) {
        let package = PythonPackage::new_discovered(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
            expected,
        );

        assert_eq!(package.is_publishable_by_default(), expected);
    }

    #[test]
    fn test_python_package_set_changed() {
        changepacks_core::assert_set_changed_roundtrip!(PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("test/pyproject.toml"),
        ));
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

    #[tokio::test]
    async fn test_python_package_update_version_preserves_newline() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            "[project]\nname = \"test-package\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        let mut package = PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.ends_with('\n'));
        assert!(content.contains(r#"version = "1.0.1""#));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_python_package_dependencies() {
        changepacks_core::assert_dependencies_roundtrip!(
            PythonPackage::new(
                Some("test-package".to_string()),
                Some("1.0.0".to_string()),
                PathBuf::from("/test/pyproject.toml"),
                PathBuf::from("test/pyproject.toml"),
            ),
            "requests",
            "core"
        );
    }

    #[tokio::test]
    async fn test_python_package_update_version_without_project_section() {
        // Regression: a pyproject.toml with only `[build-system]` (no `[project]`)
        // is a legitimate PEP 517 shape. PythonPackage::update_version must create
        // the `[project]` table and set the version, preserving `[build-system]`.
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        fs::write(
            &pyproject_toml,
            r#"[build-system]
requires = ["setuptools"]
"#,
        )
        .unwrap();

        let mut package = PythonPackage::new(
            None,
            None,
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&pyproject_toml).await.unwrap();
        assert!(content.contains("[project]"));
        assert!(content.contains("version = \"0.0.1\""));
        assert!(content.contains("[build-system]"));
        assert!(content.contains("requires = [\"setuptools\"]"));

        temp_dir.close().unwrap();
    }

    /// A `pyproject.toml` that does not parse must abort the bump BEFORE the
    /// writer touches the file. The finder covers a malformed manifest, and
    /// `write_pyproject_version` covers semantic rejections, but nothing pinned
    /// the parse failure as observed through the `Package` trait entry point —
    /// so swallowing the parse error inside the writer would still leave this
    /// path green. Mirrors
    /// `test_write_pyproject_version_non_table_project_leaves_file_untouched`.
    #[tokio::test]
    async fn test_python_package_update_version_malformed_manifest_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let pyproject_toml = temp_dir.path().join("pyproject.toml");
        let original = "invalid toml [[[";
        fs::write(&pyproject_toml, original).unwrap();

        let mut package = PythonPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            pyproject_toml.clone(),
            PathBuf::from("pyproject.toml"),
        );

        changepacks_utils::assert_malformed_manifest_rejected!(
            package.update_version(UpdateType::Patch).await,
            &pyproject_toml,
            "pyproject.toml",
            original
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_set_name() {
        changepacks_core::assert_set_name_roundtrip!(PythonPackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pyproject.toml"),
            PathBuf::from("pyproject.toml"),
        ));
    }
}
