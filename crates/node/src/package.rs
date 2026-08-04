use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Config, Language, Package, UpdateType};

// Eight-field declaration plus the shared constructor pair, identical to
// `NodeWorkspace` (see `declare_node_project!` in `lib.rs`); the extra
// `package_manager` field rules out
// `changepacks_core::declare_discovered_project!`.
crate::declare_node_project!(pub struct NodePackage);

#[async_trait]
impl Package for NodePackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // Publishability flag accessor.
    changepacks_core::impl_publishable_by_default!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        // Shared with `NodeWorkspace::update_version` (see the note on
        // `crate::bump_package_json_version` for why this is a function call
        // and not a macro).
        crate::bump_package_json_version(&mut self.version, &self.path, update_type).await
    }

    // Fixed language accessor.
    changepacks_core::impl_language!(Language::Node);

    // Node publish command defaults (runtime-detected package manager).
    crate::impl_node_publish_wiring!();

    // Dependency set accessors.
    changepacks_core::impl_dependencies_accessors!();

    async fn publish(&self, config: &Config) -> Result<changepacks_core::publish::PublishOutput> {
        crate::run_publish_for_path(
            &self.path,
            &self.relative_path,
            config,
            changepacks_core::publish::PACKAGE_DIR_NOT_FOUND,
        )
        .await
    }

    async fn dry_run_publish(
        &self,
        config: &Config,
    ) -> Result<Option<changepacks_core::publish::PublishOutput>> {
        crate::run_dry_run_publish_for_path(
            &self.path,
            &self.relative_path,
            config,
            changepacks_core::publish::PACKAGE_DIR_NOT_FOUND,
        )
        .await
    }
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

    /// `NodePackage` binding of the shared PATH-collection-failure scenario.
    async fn assert_collection_failure_prevents_command(dry_run: bool) {
        crate::test_util::assert_collection_failure_prevents_command(
            dry_run,
            async |package_json, config| {
                let package = NodePackage::new(
                    Some("test-package".to_string()),
                    Some("1.0.0".to_string()),
                    package_json,
                    PathBuf::from("package.json"),
                );
                if dry_run {
                    package.dry_run_publish(&config).await.map(|_| ())
                } else {
                    package.publish(&config).await.map(|_| ())
                }
            },
        )
        .await;
    }

    #[test]
    fn test_node_package_new() {
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
        assert!(!package.is_changed());
        assert!(package.is_publishable_by_default());
        assert_eq!(package.default_publish_command(), "npm publish");
        assert_eq!(
            package.default_dry_run_publish_command().as_deref(),
            Some("npm publish --dry-run")
        );
    }

    #[rstest]
    #[case(true)]
    #[case(false)]
    fn test_node_package_discovered_publishability(#[case] expected: bool) {
        let package = NodePackage::new_discovered(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
            crate::PackageManager::Npm,
            expected,
        );

        assert_eq!(package.is_publishable_by_default(), expected);
    }

    #[test]
    fn test_node_package_set_changed() {
        changepacks_core::assert_set_changed_roundtrip!(NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
        ));
    }

    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_node_package_update_version(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
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

        package.update_version(update_type).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(&format!(r#""version": "{expected}""#)));

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
        assert!(content.contains(r#""version":"1.0.1""#));

        temp_dir.close().unwrap();
    }

    /// A `package.json` with no `version` key at all must gain one instead of
    /// being left untouched. This drives the `obj.insert("version", ...)` arm
    /// of `crate::write_package_json_version` through the `Package` trait
    /// surface - the only other route to that arm is `NodeWorkspace`, so a
    /// writer that silently skipped a manifest without a `version` field would
    /// still leave every `NodePackage` test green. `next_version_or_default`
    /// starts from `0.0.0`, so `Patch` must land on `0.0.1`. Mirrors
    /// `test_python_package_update_version_without_project_section` and
    /// `test_update_version_without_property_group_creates_global_version`.
    #[tokio::test]
    async fn test_node_package_update_version_adds_missing_version_field() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
  "name": "test-package"
}
"#,
        )
        .unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            None,
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(
            content.contains(r#""version": "0.0.1""#),
            "a missing version field must be inserted, got: {content}"
        );
        assert!(
            content.contains(r#""name": "test-package""#),
            "the existing fields must survive the insert, got: {content}"
        );
        assert!(
            content.ends_with("}\n"),
            "the trailing newline shape must be preserved, got: {content}"
        );
        assert_eq!(package.version(), Some("0.0.1"));

        temp_dir.close().unwrap();
    }

    /// A `package.json` that does not parse must abort the bump BEFORE the
    /// writer touches the file. The finder covers a malformed manifest
    /// (`test_node_project_finder_visit_malformed_package_json`), but nothing
    /// pinned the parse failure as observed through the `Package` trait entry
    /// point - so a writer that swallowed the parse error and rewrote a
    /// truncated manifest would still leave this path green. Mirrors
    /// `test_python_package_update_version_malformed_manifest_leaves_file_untouched`.
    #[tokio::test]
    async fn test_node_package_update_version_malformed_manifest_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        let original = r#"{ "name": "test-package", invalid json }"#;
        fs::write(&package_json, original).unwrap();

        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        changepacks_utils::assert_malformed_manifest_rejected!(
            package.update_version(UpdateType::Patch).await,
            &package_json,
            "package.json",
            original
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_set_name() {
        changepacks_core::assert_set_name_roundtrip!(NodePackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("package.json"),
        ));
    }

    #[tokio::test]
    async fn test_node_modules_bin_dirs_async_includes_node_modules_bin() {
        let temp_dir = TempDir::new().unwrap();
        let bin = temp_dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin).unwrap();
        // Behavior lock: the async publish path (`run_publish_for_path` ->
        // `node_modules_bin_dirs_async`) surfaces the package's local
        // node_modules/.bin so lifecycle hooks (husky) resolve during publish /
        // dry-run. NodePackage / NodeWorkspace route PATH wiring through this
        // async collector; the core trait defaults pass no extra PATH dirs.
        // depth 1 covers the package dir itself, where `node_modules/.bin` lives.
        let dirs = crate::node_modules_bin_dirs_async(temp_dir.path(), 1)
            .await
            .unwrap();
        assert!(dirs.contains(&bin));
    }

    #[tokio::test]
    async fn test_publish_stops_when_path_collection_fails() {
        assert_collection_failure_prevents_command(false).await;
    }

    #[tokio::test]
    async fn test_dry_run_publish_stops_when_path_collection_fails() {
        assert_collection_failure_prevents_command(true).await;
    }
}
