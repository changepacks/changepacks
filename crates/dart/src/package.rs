use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};

// Seven-field discovered-project declaration plus `new` / `new_discovered`,
// shared verbatim with the other four identical language types.
changepacks_core::declare_discovered_project!(pub struct DartPackage);

#[async_trait]
impl Package for DartPackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // Publishability flag accessor.
    changepacks_core::impl_publishable_by_default!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::bump_pubspec_version(&mut self.version, &self.path, update_type).await
    }

    // Fixed language accessor.
    changepacks_core::impl_language!(Language::Dart);

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
    use rstest::rstest;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_new() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
"#,
        )
        .unwrap();

        let package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert_eq!(package.name(), Some("test_package"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), pubspec_path);
        assert_eq!(package.relative_path(), PathBuf::from("pubspec.yaml"));
        assert!(!package.is_changed());
        assert!(package.is_publishable_by_default());
        assert_eq!(package.language(), Language::Dart);
        assert_eq!(package.default_publish_command(), "dart pub publish");
        assert_eq!(
            package.default_dry_run_publish_command().as_deref(),
            Some("dart pub publish --dry-run")
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_new_discovered_carries_default_publishability() {
        let package = DartPackage::new_discovered(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("pubspec.yaml"),
            false,
        );

        assert!(!package.is_publishable_by_default());
    }

    #[test]
    fn test_set_changed() {
        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("pubspec.yaml"),
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
    async fn test_update_version(#[case] update_type: UpdateType, #[case] expected: &str) {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_package
version: 1.0.0
"#,
        )
        .unwrap();

        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        package.update_version(update_type).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains(&format!("version: {expected}")));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_preserves_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        let original_content = r#"name: test_package
version: 1.0.0
description: A test package
dependencies:
  http: ^1.0.0
"#;
        fs::write(&pubspec_path, original_content).unwrap();

        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 1.0.1"));
        assert!(content.contains("name: test_package"));
        assert!(content.contains("description: A test package"));
        assert!(content.contains("dependencies:"));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_dart_package_update_version_preserves_newline() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(&pubspec_path, "name: test_package\nversion: 1.0.0\n").unwrap();

        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.ends_with('\n'));
        assert!(content.contains("version: 1.0.1"));

        temp_dir.close().unwrap();
    }

    /// A `pubspec.yaml` that does not parse must abort the bump BEFORE the
    /// writer touches the file. The finder covers a malformed manifest
    /// (`test_visit_malformed_pubspec_yaml`), but nothing pinned the
    /// `yamlpath` parse failure as observed through the `Package` trait entry
    /// point — so a writer that swallowed the parse error and rewrote the file
    /// from scratch would still leave this path green. Mirrors
    /// `test_python_package_update_version_malformed_manifest_leaves_file_untouched`.
    #[tokio::test]
    async fn test_dart_package_update_version_malformed_manifest_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        let original = "name: test_package\nversion: [1.0.0\n";
        fs::write(&pubspec_path, original).unwrap();

        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        let err = package
            .update_version(UpdateType::Patch)
            .await
            .expect_err("a malformed pubspec.yaml must fail the bump");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("Failed to parse pubspec.yaml"),
            "error chain should name the parse failure, got: {chain}"
        );
        assert!(
            chain.contains(&pubspec_path.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );

        // Byte-for-byte: an unparseable manifest must never be rewritten.
        assert_eq!(
            fs::read(&pubspec_path).unwrap(),
            original.as_bytes(),
            "a rejected bump must leave the manifest byte-identical"
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_dependencies() {
        let mut package = DartPackage::new(
            Some("test_package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("test/pubspec.yaml"),
        );

        // Initially empty
        assert!(package.dependencies().is_empty());

        // Add dependencies
        package.add_dependency("http");
        package.add_dependency("core");

        let deps = package.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("http"));
        assert!(deps.contains("core"));

        // Adding duplicate should not increase count
        package.add_dependency("http");
        assert_eq!(package.dependencies().len(), 2);
    }

    #[test]
    fn test_set_name() {
        changepacks_core::assert_set_name_roundtrip!(DartPackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("pubspec.yaml"),
        ));
    }
}
