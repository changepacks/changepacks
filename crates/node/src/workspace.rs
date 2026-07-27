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
        let path = &self.path;
        changepacks_utils::bump_version_with(&mut self.version, path, update_type, async |new| {
            crate::write_package_json_version(path, new).await
        })
        .await
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
    use crate::test_util::{denied_metadata, marker_command};
    use changepacks_core::UpdateType;
    use rstest::rstest;
    use std::fs;
    use tempfile::TempDir;
    use tokio::fs::read_to_string;

    async fn assert_collection_failure_prevents_command(dry_run: bool) {
        let temp_dir = TempDir::new().unwrap();
        let package_json = temp_dir.path().join("package.json");
        fs::write(&package_json, "{}").unwrap();

        let workspace = NodeWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            package_json,
            PathBuf::from("package.json"),
        );
        let marker_name = if dry_run {
            "dry-run-command-invoked"
        } else {
            "publish-command-invoked"
        };
        let marker = temp_dir.path().join(marker_name);
        let mut config = Config::default();
        let commands = if dry_run {
            &mut config.publish_dry_run
        } else {
            &mut config.publish
        };
        commands.insert("package.json".to_string(), marker_command(marker_name));

        let result = if dry_run {
            crate::with_test_metadata_probe(denied_metadata, workspace.dry_run_publish(&config))
                .await
                .map(|_| ())
        } else {
            crate::with_test_metadata_probe(denied_metadata, workspace.publish(&config))
                .await
                .map(|_| ())
        };
        let error = result.expect_err("PATH collection must fail before command execution");
        let chain = format!("{error:#}");
        assert!(
            chain.contains("node_modules\\.bin") || chain.contains("node_modules/.bin"),
            "error should name the candidate .bin path, got: {chain}"
        );
        assert!(chain.contains("deterministic metadata failure"));
        assert!(!marker.exists(), "configured command must not be invoked");
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
        let mut workspace = NodeWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/package.json"),
            PathBuf::from("package.json"),
        );
        assert_eq!(workspace.name(), None);
        workspace.set_name("my-project".to_string());
        assert_eq!(workspace.name(), Some("my-project"));
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
