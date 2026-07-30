use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct RustPackage {
    name: Option<String>,
    version: Option<String>,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
    dependencies: HashSet<String>,
    publishable_by_default: bool,
    workspace_version_inherited: bool,
    workspace_root: Option<PathBuf>,
}

impl RustPackage {
    /// Single construction site for every `RustPackage` field default.
    ///
    /// `new` and `new_with_workspace_version` used to spell out the same
    /// nine-field struct literal and differed only in
    /// `workspace_version_inherited` and `workspace_root`, so adding a field
    /// meant editing both in lock-step. The other language crates collapse
    /// this shape with a macro (`changepacks_core::impl_discovered_new`,
    /// `crate::impl_node_discovered_new`, `crate::impl_gradle_constructors`);
    /// `RustPackage`'s two extra fields rule those out, so it delegates here
    /// instead.
    fn build(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        workspace_version_inherited: bool,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
            dependencies: HashSet::new(),
            publishable_by_default: true,
            workspace_version_inherited,
            workspace_root,
        }
    }

    #[must_use]
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self::build(name, version, path, relative_path, false, None)
    }

    #[must_use]
    pub fn new_with_workspace_version(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self::build(name, version, path, relative_path, true, workspace_root)
    }

    #[must_use]
    pub(crate) fn with_publishable_by_default(mut self, publishable_by_default: bool) -> Self {
        self.publishable_by_default = publishable_by_default;
        self
    }
}

#[async_trait]
impl Package for RustPackage {
    // Seven basic accessors (`name`, `version`, `path`, `relative_path`,
    // `is_changed`, `set_changed`, `set_name`) share their byte-identical
    // bodies with every other language crate's `Package` / `Workspace`
    // impl. Consolidated via `impl_basic_accessors!()` in `changepacks-core`
    // — expansion is byte-identical to the previous hand-rolled bodies.
    // Rust-specific overrides (`is_publishable_by_default`,
    // `inherits_workspace_version`, `workspace_root_path`) stay hand-rolled
    // below.
    changepacks_core::impl_basic_accessors!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        // Members that inherit `version.workspace = true` from the workspace
        // root MUST NOT rewrite their own `[package].version` here —
        // `write_cargo_package_version` would clobber the inheritance
        // marker with a plain string, silently breaking the workspace-wide
        // version link. The bump for those members is owned by the
        // workspace root's `RustWorkspace::update_version`, which drives
        // `[workspace.package].version` (and fans out into workspace
        // dependencies). For inheriting members, the correct action here
        // is a no-op.
        if self.workspace_version_inherited {
            return Ok(());
        }
        let path = &self.path;
        changepacks_utils::bump_version_with(&mut self.version, path, update_type, async |new| {
            crate::write_cargo_package_version(path, new).await
        })
        .await
    }

    // Byte-identical `fn language(&self) -> Language { Language::Rust }`
    // one-liner shared with every other language crate's `Package` /
    // `Workspace` impl. Consolidated via `impl_language!()` in
    // `changepacks-core` alongside the other accessor macros.
    changepacks_core::impl_language!(Language::Rust);

    // Publishability flag accessor.
    changepacks_core::impl_publishable_by_default!();

    // `default_publish_command` / `default_dry_run_publish_command` share
    // their const-based shape with every other const-driven language
    // crate. Consolidated via `impl_const_publish_commands!()` in
    // `changepacks-core` — expansion is byte-identical to the previous
    // hand-rolled bodies. The macro's `$publish:path` argument keeps the
    // `crate::PUBLISH_COMMAND` / `crate::DRY_RUN_PUBLISH_COMMAND` const
    // choice explicit at this call site, matching the pattern used by
    // Python/Dart/Java package impls.
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

    fn inherits_workspace_version(&self) -> bool {
        self.workspace_version_inherited
    }

    fn workspace_root_path(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
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
    async fn test_rust_package_new() {
        let package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(package.name(), Some("test-package"));
        assert_eq!(package.version(), Some("1.0.0"));
        assert_eq!(package.path(), PathBuf::from("/test/Cargo.toml"));
        assert_eq!(package.relative_path(), PathBuf::from("test/Cargo.toml"));
        assert_eq!(package.language(), Language::Rust);
        assert!(!package.is_changed());
        assert_eq!(package.default_publish_command(), "cargo publish");
        assert_eq!(
            package.default_dry_run_publish_command().as_deref(),
            Some("cargo publish --dry-run")
        );
    }

    #[tokio::test]
    async fn test_rust_package_set_changed() {
        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
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
    async fn test_rust_package_update_version(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        package.update_version(update_type).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains(&format!("version = \"{expected}\"")));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_package_update_version_preserves_formatting() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[package]
name = "test-package"
version = "1.2.3"
edition = "2021"

[dependencies]
tokio = "1.0"
"#,
        )
        .unwrap();

        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.2.3".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"1.2.4\""));
        assert!(content.contains("name = \"test-package\""));
        assert!(content.contains("edition = \"2021\""));
        assert!(content.contains("[dependencies]"));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_rust_package_dependencies() {
        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        // Initially empty
        assert!(package.dependencies().is_empty());

        // Add dependencies
        package.add_dependency("core");
        package.add_dependency("utils");

        let deps = package.dependencies();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains("core"));
        assert!(deps.contains("utils"));

        // Adding duplicate should not increase count
        package.add_dependency("core");
        assert_eq!(package.dependencies().len(), 2);
    }

    #[test]
    fn test_rust_package_inherits_workspace_version() {
        let package = RustPackage::new(
            Some("test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );
        assert!(!package.inherits_workspace_version());
        assert!(package.workspace_root_path().is_none());
    }

    #[test]
    fn test_rust_package_with_workspace_version() {
        let package = RustPackage::new_with_workspace_version(
            Some("test".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/crates/foo/Cargo.toml"),
            PathBuf::from("crates/foo/Cargo.toml"),
            Some(PathBuf::from("/test/Cargo.toml")),
        );
        assert!(package.inherits_workspace_version());
        assert_eq!(
            package.workspace_root_path(),
            Some(Path::new("/test/Cargo.toml"))
        );
    }

    #[test]
    fn test_set_name() {
        changepacks_core::assert_set_name_roundtrip!(RustPackage::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("Cargo.toml"),
        ));
    }

    #[tokio::test]
    async fn test_rust_package_update_version_preserves_workspace_inheritance() {
        // Regression: `RustPackage::update_version` used to unconditionally
        // rewrite `[package].version = "..."`. On a member that inherits its
        // version from the workspace root (`version.workspace = true`), that
        // silently clobbered the inheritance marker with a plain string —
        // permanently detaching the member from the workspace-wide bump.
        // The correct action for an inheriting member is a no-op here; the
        // workspace root owns the bump.
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        let original_content = r#"[package]
name = "test-inherited"
version.workspace = true
edition = "2024"

[dependencies]
"#;
        fs::write(&cargo_toml, original_content).unwrap();

        let mut package = RustPackage::new_with_workspace_version(
            Some("test-inherited".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("crates/test-inherited/Cargo.toml"),
            Some(PathBuf::from("/test/Cargo.toml")),
        );

        package.update_version(UpdateType::Patch).await.unwrap();

        // The on-disk manifest still declares the workspace-inherited
        // marker — no rewritten `version = "X.Y.Z"` line.
        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(
            content.contains("version.workspace = true"),
            "inheritance marker was clobbered: {content}"
        );
        assert!(
            !content.contains("version = \""),
            "member Cargo.toml gained a hardcoded version string: {content}"
        );
        // `self.version` remains the originally-observed inherited value —
        // NOT the bumped `1.0.1` — because the workspace root owns the bump
        // and will reflect it back through `new_with_workspace_version`.
        assert_eq!(package.version(), Some("1.0.0"));
    }

    #[tokio::test]
    async fn test_rust_package_update_version_bump_error_includes_path() {
        // A malformed on-disk version must fail the bump and name the
        // offending manifest — completing this crate's path-in-error-context
        // pattern (the read/write around the bump already carry it). The bump
        // errors BEFORE any file write, so no on-disk fixture is needed.
        let manifest = PathBuf::from("/nonexistent/rustpkg-bump/Cargo.toml");
        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("abc".to_string()),
            manifest.clone(),
            PathBuf::from("rustpkg-bump/Cargo.toml"),
        );
        let err = package
            .update_version(UpdateType::Patch)
            .await
            .expect_err("a malformed version must fail the bump");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&manifest.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
    }

    /// A `Cargo.toml` that does not parse must abort the bump BEFORE the writer
    /// touches the file. The sibling test above only covers a malformed version
    /// STRING (which fails before any file access at all), and `lib.rs` covers
    /// the writer's semantic rejections — nothing pinned an UNPARSEABLE
    /// manifest as observed through the `Package` trait entry point, so
    /// swallowing the parse error inside `write_cargo_package_version` would
    /// still leave this path green. Twin of
    /// `test_python_package_update_version_malformed_manifest_leaves_file_untouched`.
    #[tokio::test]
    async fn test_rust_package_update_version_malformed_manifest_leaves_file_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        let original = "invalid toml [[[";
        fs::write(&cargo_toml, original).unwrap();

        let mut package = RustPackage::new(
            Some("test-package".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        let err = package
            .update_version(UpdateType::Patch)
            .await
            .expect_err("a malformed Cargo.toml must fail the bump");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("Failed to parse Cargo.toml"),
            "error chain should name the parse failure, got: {chain}"
        );
        assert!(
            chain.contains(&cargo_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );

        // Byte-for-byte: an unparseable manifest must never be rewritten.
        assert_eq!(
            fs::read(&cargo_toml).unwrap(),
            original.as_bytes(),
            "a rejected bump must leave the manifest byte-identical"
        );
        assert_eq!(
            package.version(),
            Some("1.0.0"),
            "a rejected bump must not advance the in-memory version"
        );

        temp_dir.close().unwrap();
    }
}
