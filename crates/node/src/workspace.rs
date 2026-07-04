use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use std::collections::HashSet;
use std::path::PathBuf;

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
    // Byte-identical `#[must_use] pub fn new(name, version, path,
    // relative_path)` constructor body shared with every other
    // "plain 5-basic-field" language crate's `Package` / `Workspace`.
    // Consolidated via `impl_default_new!()` in `changepacks-core` — see
    // that macro's doc for the exact struct-field contract.
    changepacks_core::impl_default_new!();
}

#[async_trait]
impl Workspace for NodeWorkspace {
    // Seven basic accessors (`name`, `version`, `path`, `relative_path`,
    // `is_changed`, `set_changed`, `set_name`) share their byte-identical
    // bodies with every other language crate's `Package` / `Workspace`
    // impl. Consolidated via `impl_basic_accessors!()` in `changepacks-core`
    // — expansion is byte-identical to the previous hand-rolled bodies.
    changepacks_core::impl_basic_accessors!();

    // `update_version` shares its byte-identical body with `NodePackage`.
    // Consolidated via the shared `update_version_from_fields` helper in
    // `crates/node/src/lib.rs` so the "reserve `0.0.0`" fallback lives in
    // ONE place. See the helper's doc comment for why a `macro_rules!`
    // producing `async fn` is incompatible with `#[async_trait]` (E0195
    // lifetime mismatch).
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::update_version_from_fields(&mut self.version, &self.path, update_type).await
    }

    // Byte-identical `fn language(&self) -> Language { Language::Node }`
    // one-liner shared with every other language crate's `Package` /
    // `Workspace` impl. Consolidated via `impl_language!()` in
    // `changepacks-core` alongside the other accessor macros.
    changepacks_core::impl_language!(Language::Node);

    // `default_publish_command`, `default_dry_run_publish_command`, and
    // `publish_path_dirs` share their byte-identical bodies with
    // `NodePackage`. Consolidated via `impl_node_publish_wiring!()` in
    // `crates/node/src/lib.rs` — expansion is byte-identical to the
    // previous hand-rolled bodies.
    crate::impl_node_publish_wiring!();

    // `dependencies()` / `add_dependency()` share their byte-identical
    // body with every other language crate's `Package` and `Workspace`
    // impl (all use `dependencies: HashSet<String>` as their backing
    // store). Consolidated via the `impl_dependencies_accessors!()`
    // macro in `changepacks-core` so future accessor tweaks land in
    // one place — expansion is byte-identical to the previous
    // hand-rolled bodies.
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
        assert_eq!(workspace.default_publish_command(), "npm publish");
        assert_eq!(
            workspace.default_dry_run_publish_command().as_deref(),
            Some("npm publish --dry-run")
        );
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
}
