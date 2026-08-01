use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};

// Seven-field discovered-project declaration plus `new` / `new_discovered`,
// shared verbatim with the other four identical language types.
changepacks_core::declare_discovered_project!(pub struct DartWorkspace);

#[async_trait]
impl Workspace for DartWorkspace {
    // Seven basic accessors (`name`, `version`, `path`, `relative_path`,
    // `is_changed`, `set_changed`, `set_name`) share their byte-identical
    // bodies with every other language crate's `Package` / `Workspace`
    // impl. Consolidated via `impl_basic_accessors!()` in `changepacks-core`
    // — expansion is byte-identical to the previous hand-rolled bodies.
    changepacks_core::impl_basic_accessors!();

    // Publishability flag accessor.
    changepacks_core::impl_publishable_by_default!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        crate::bump_pubspec_version(&mut self.version, &self.path, update_type).await
    }

    // Byte-identical `fn language(&self) -> Language { Language::Dart }`
    // one-liner shared with every other language crate's `Package` /
    // `Workspace` impl. Consolidated via `impl_language!()` in
    // `changepacks-core` alongside the other accessor macros.
    changepacks_core::impl_language!(Language::Dart);

    // `default_publish_command` / `default_dry_run_publish_command` share
    // their const-based shape with every other const-driven language
    // crate. Consolidated via `impl_const_publish_commands!()` in
    // `changepacks-core` — expansion is byte-identical to the previous
    // hand-rolled bodies.
    changepacks_core::impl_const_publish_commands!(
        crate::PUBLISH_COMMAND,
        crate::DRY_RUN_PUBLISH_COMMAND
    );

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
    use rstest::rstest;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_new_with_name_and_version() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
version: 1.0.0
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert_eq!(workspace.name(), Some("test_workspace"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), pubspec_path);
        assert_eq!(workspace.relative_path(), PathBuf::from("pubspec.yaml"));
        assert!(!workspace.is_changed());
        assert!(workspace.is_publishable_by_default());
        assert_eq!(workspace.language(), Language::Dart);
        assert_eq!(workspace.default_publish_command(), "dart pub publish");
        assert_eq!(
            workspace.default_dry_run_publish_command().as_deref(),
            Some("dart pub publish --dry-run")
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_new_discovered_carries_default_publishability() {
        let workspace = DartWorkspace::new_discovered(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("pubspec.yaml"),
            false,
        );

        assert!(!workspace.is_publishable_by_default());
    }

    #[test]
    fn test_new_without_name_and_version() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let workspace = DartWorkspace::new(
            None,
            None,
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
        assert_eq!(workspace.path(), pubspec_path);
        assert!(!workspace.is_changed());

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_set_changed() {
        changepacks_core::assert_set_changed_roundtrip!(DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("pubspec.yaml"),
        ));
    }

    #[rstest]
    #[case(UpdateType::Patch, "1.0.1")]
    #[case(UpdateType::Minor, "1.1.0")]
    #[case(UpdateType::Major, "2.0.0")]
    #[tokio::test]
    async fn test_update_version_with_existing_version(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
version: 1.0.0
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        workspace.update_version(update_type).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains(&format!("version: {expected}")));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_update_version_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        fs::write(
            &pubspec_path,
            r#"name: test_workspace
workspace:
  packages:
    - packages/*
"#,
        )
        .unwrap();

        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            None,
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = fs::read_to_string(&pubspec_path).unwrap();
        assert!(content.contains("version: 0.0.1"));

        temp_dir.close().unwrap();
    }

    /// Workspace counterpart of
    /// `test_dart_package_update_version_malformed_manifest_leaves_file_untouched`.
    /// `DartWorkspace` had no error-path coverage at all, so a `yamlpath` parse
    /// failure reached through the `Workspace` trait entry point was unpinned:
    /// nothing stopped a future writer from truncating or half-writing a
    /// manifest it could not parse.
    #[tokio::test]
    async fn test_dart_workspace_update_version_malformed_manifest_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let pubspec_path = temp_dir.path().join("pubspec.yaml");
        let original = "name: test_workspace\nversion: [1.0.0\n";
        fs::write(&pubspec_path, original).unwrap();

        let mut workspace = DartWorkspace::new(
            Some("test_workspace".to_string()),
            Some("1.0.0".to_string()),
            pubspec_path.clone(),
            PathBuf::from("pubspec.yaml"),
        );

        changepacks_utils::assert_malformed_manifest_rejected!(
            workspace.update_version(UpdateType::Patch).await,
            &pubspec_path,
            "pubspec.yaml",
            original
        );

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_dependencies() {
        changepacks_core::assert_dependencies_roundtrip!(
            DartWorkspace::new(
                Some("test_workspace".to_string()),
                Some("1.0.0".to_string()),
                PathBuf::from("/test/pubspec.yaml"),
                PathBuf::from("test/pubspec.yaml"),
            ),
            "http",
            "core"
        );
    }

    #[test]
    fn test_set_name() {
        changepacks_core::assert_set_name_roundtrip!(DartWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/pubspec.yaml"),
            PathBuf::from("pubspec.yaml"),
        ));
    }
}
