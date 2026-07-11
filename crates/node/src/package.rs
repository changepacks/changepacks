use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Config, Language, Package, UpdateType};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug)]
pub struct NodePackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl NodePackage {
    // Standard package/workspace constructor.
    changepacks_core::impl_default_new!();
}

#[async_trait]
impl Package for NodePackage {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // `update_version` shares its byte-identical body with `NodeWorkspace`.
    // Consolidated via the shared `update_version_from_fields` helper in
    // `crates/node/src/lib.rs` so the "reserve `0.0.0`" fallback lives in
    // ONE place. See the helper's doc comment for why a `macro_rules!`
    // producing `async fn` is incompatible with `#[async_trait]` (E0195
    // lifetime mismatch).
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::update_version_from_fields(&mut self.version, &self.path, update_type).await
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
        assert!(!package.is_changed());
        assert_eq!(package.default_publish_command(), "npm publish");
        assert_eq!(
            package.default_dry_run_publish_command().as_deref(),
            Some("npm publish --dry-run")
        );
    }

    #[tokio::test]
    async fn test_node_package_set_changed() {
        let mut package = NodePackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
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
        assert!(content.contains(r#""version": "1.0.1""#));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_set_name() {
        let mut package = NodePackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("package.json"),
        );
        assert_eq!(package.name(), None);
        package.set_name("my-project".to_string());
        assert_eq!(package.name(), Some("my-project"));
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
        let dirs = crate::node_modules_bin_dirs_async(temp_dir.path()).await;
        assert!(dirs.contains(&bin));
    }
}
