use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Config, Language, UpdateType, Workspace};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug)]
pub struct NodeWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    publishable_by_default: bool,
    dependencies: HashSet<String>,
    pub(crate) package_manager: crate::PackageManager,
}

impl NodeWorkspace {
    // Constructors shared with `NodePackage` (extra `package_manager` field
    // rules out `changepacks_core::impl_discovered_new!`).
    crate::impl_node_discovered_new!();
}

#[async_trait]
impl Workspace for NodeWorkspace {
    // Standard package/workspace accessors.
    changepacks_core::impl_basic_accessors!();

    // Publishability flag accessor.
    changepacks_core::impl_publishable_by_default!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        // Shared with `NodePackage::update_version` (see the note on
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
            changepacks_core::publish::WORKSPACE_DIR_NOT_FOUND,
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
            changepacks_core::publish::WORKSPACE_DIR_NOT_FOUND,
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

    /// `NodeWorkspace` binding of the shared PATH-collection-failure scenario.
    async fn assert_collection_failure_prevents_command(dry_run: bool) {
        crate::test_util::assert_collection_failure_prevents_command(
            dry_run,
            async |package_json, config| {
                let workspace = NodeWorkspace::new(
                    Some("test-workspace".to_string()),
                    Some("1.0.0".to_string()),
                    package_json,
                    PathBuf::from("package.json"),
                );
                if dry_run {
                    workspace.dry_run_publish(&config).await.map(|_| ())
                } else {
                    workspace.publish(&config).await.map(|_| ())
                }
            },
        )
        .await;
    }

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
        assert!(!workspace.is_changed());
        assert!(workspace.is_publishable_by_default());
        assert_eq!(workspace.default_publish_command(), "npm publish");
        assert_eq!(
            workspace.default_dry_run_publish_command().as_deref(),
            Some("npm publish --dry-run")
        );
    }

    #[rstest]
    #[case(true)]
    #[case(false)]
    fn test_node_workspace_discovered_publishability(#[case] expected: bool) {
        let workspace = NodeWorkspace::new_discovered(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("test/package.json"),
            crate::PackageManager::Npm,
            expected,
        );

        assert_eq!(workspace.is_publishable_by_default(), expected);
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
    async fn test_node_workspace_update_version_with_existing_version(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
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

        workspace.update_version(update_type).await.unwrap();

        let content = read_to_string(&package_json).await.unwrap();
        assert!(content.contains(&format!(r#""version": "{expected}""#)));

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

    /// Workspace-side twin of the `NodePackage` malformed-manifest test: the
    /// two `update_version` bodies share `bump_package_json_version`, so both
    /// trait entry points must be pinned independently or a regression could be
    /// hidden behind whichever one is still covered.
    #[tokio::test]
    async fn test_node_workspace_update_version_malformed_manifest_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        let original = r#"{ "name": "test-workspace", invalid json }"#;
        fs::write(&package_json, original).unwrap();

        let mut workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            package_json.clone(),
            PathBuf::from("package.json"),
        );

        let err = workspace
            .update_version(UpdateType::Patch)
            .await
            .expect_err("a malformed package.json must fail the bump");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("Failed to parse package.json"),
            "error chain should name the parse failure, got: {chain}"
        );
        assert!(
            chain.contains(&package_json.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );

        // Byte-for-byte: an unparseable manifest must never be rewritten.
        assert_eq!(
            fs::read(&package_json).unwrap(),
            original.as_bytes(),
            "a rejected bump must leave the manifest byte-identical"
        );

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

    #[test]
    fn test_set_name() {
        changepacks_core::assert_set_name_roundtrip!(NodeWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("package.json"),
        ));
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
