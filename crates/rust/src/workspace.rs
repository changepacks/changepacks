use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType, Workspace};
use changepacks_utils::{finalize_content, next_version_or_default, split_version};
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::fs::{read_to_string, write};
use toml_edit::DocumentMut;

#[derive(Debug)]
pub struct RustWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    dependencies: HashSet<String>,
}

impl RustWorkspace {
    // Byte-identical `#[must_use] pub fn new(name, version, path,
    // relative_path)` constructor body shared with every other
    // "plain 5-basic-field" language crate's `Package` / `Workspace`.
    // Consolidated via `impl_default_new!()` in `changepacks-core` — see
    // that macro's doc for the exact struct-field contract.
    changepacks_core::impl_default_new!();
}

#[async_trait]
impl Workspace for RustWorkspace {
    // Seven basic accessors (`name`, `version`, `path`, `relative_path`,
    // `is_changed`, `set_changed`, `set_name`) share their byte-identical
    // bodies with every other language crate's `Package` / `Workspace`
    // impl. Consolidated via `impl_basic_accessors!()` in `changepacks-core`
    // — expansion is byte-identical to the previous hand-rolled bodies.
    changepacks_core::impl_basic_accessors!();

    async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        // Hoisted `unwrap_or("0.0.0")` so the "reserve 0.0.0 when
        // unversioned" fallback is expressed ONCE, then reused BOTH for
        // `next_version_or_default` here AND for the
        // `[workspace.dependencies]` sync at the loop below. Matches the
        // shared policy consolidated in `changepacks_utils::next_version_or_default`.
        let old_version = self.version.as_deref().unwrap_or("0.0.0");
        let new_version = next_version_or_default(Some(old_version), update_type)?;

        let cargo_toml_raw = read_to_string(&self.path).await?;
        let mut cargo_toml: DocumentMut = cargo_toml_raw.parse::<DocumentMut>()?;

        let has_package = cargo_toml.get("package").is_some();
        let has_workspace_package_version = cargo_toml
            .get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("version"))
            .is_some();

        let fallback_name = self.name.as_deref().unwrap_or("_");
        if has_package {
            cargo_toml["package"]["version"] = new_version.clone().into();
            if cargo_toml["package"].get("name").is_none() {
                cargo_toml["package"]["name"] = fallback_name.into();
            }
        } else if !has_workspace_package_version {
            // No [package] and no [workspace.package].version — create [package]
            cargo_toml["package"] = toml_edit::Item::Table(toml_edit::Table::new());
            cargo_toml["package"]["version"] = new_version.clone().into();
            cargo_toml["package"]["name"] = fallback_name.into();
        }
        // else: virtual workspace — only [workspace.package].version needs updating (below)

        // Update [workspace.package].version if it exists
        if let Some(ws_pkg) = cargo_toml
            .get_mut("workspace")
            .and_then(|w| w.get_mut("package"))
            .and_then(|p| p.as_table_mut())
            && ws_pkg.contains_key("version")
        {
            ws_pkg["version"] = toml_edit::value(new_version.clone());
        }

        // Sync [workspace.dependencies] for local path deps whose version matched
        // the old workspace version (these are workspace members bumped together)
        if let Some(ws_deps) = cargo_toml
            .get_mut("workspace")
            .and_then(|w| w.get_mut("dependencies"))
            .and_then(|d| d.as_table_mut())
        {
            // `old_version` hoisted to the top of this function so both the
            // `next_version_or_default` fallback and this workspace-deps
            // sync share the same "reserve 0.0.0 when unversioned" source.
            for (_, value) in ws_deps.iter_mut() {
                // `split_version` no longer returns `Result` (it was total —
                // both match arms returned `Ok`), so the destructure moves
                // out of the `&&`-let-chain into a plain irrefutable `let`
                // followed by a `ver == old_version` guard. The refutable
                // gates that still filter the branch (`as_inline_table_mut`,
                // `dep.get("path").is_some()`, `dep.get("version").as_str()`)
                // stay in the `&&`-chain unchanged.
                if let Some(dep) = value.as_inline_table_mut()
                    && dep.get("path").is_some()
                    && let Some(ver_str) = dep.get("version").and_then(|v| v.as_str())
                {
                    let (prefix, ver) = split_version(ver_str);
                    if ver == old_version {
                        dep["version"] =
                            format!("{}{new_version}", prefix.unwrap_or_default()).into();
                    }
                }
            }
        }

        write(
            &self.path,
            finalize_content(&cargo_toml.to_string(), &cargo_toml_raw),
        )
        .await?;
        self.version = Some(new_version);
        Ok(())
    }

    // Byte-identical `fn language(&self) -> Language { Language::Rust }`
    // one-liner shared with every other language crate's `Package` /
    // `Workspace` impl. Consolidated via `impl_language!()` in
    // `changepacks-core` alongside the other accessor macros.
    changepacks_core::impl_language!(Language::Rust);

    // `default_publish_command` / `default_dry_run_publish_command` share
    // their const-based shape with every other const-driven language
    // crate. Consolidated via `impl_const_publish_commands!()` in
    // `changepacks-core` — expansion is byte-identical to the previous
    // hand-rolled bodies. The macro's `$publish:path` argument keeps the
    // workspace-scoped `crate::WORKSPACE_PUBLISH_COMMAND` /
    // `crate::WORKSPACE_DRY_RUN_PUBLISH_COMMAND` const choice explicit at
    // this call site, matching the pattern used by Python/Dart/Java
    // workspace impls.
    changepacks_core::impl_const_publish_commands!(
        crate::WORKSPACE_PUBLISH_COMMAND,
        crate::WORKSPACE_DRY_RUN_PUBLISH_COMMAND
    );

    // `dependencies()` / `add_dependency()` share their byte-identical
    // body with every other language crate's `Package` and `Workspace`
    // impl (all use `dependencies: HashSet<String>` as their backing
    // store). Consolidated via the `impl_dependencies_accessors!()`
    // macro in `changepacks-core` so future accessor tweaks land in
    // one place — expansion is byte-identical to the previous
    // hand-rolled bodies.
    changepacks_core::impl_dependencies_accessors!();

    async fn update_workspace_dependencies(&self, packages: &[&dyn Package]) -> Result<()> {
        // Fast-path: if the caller feeds a cross-language `packages` slice
        // with zero eligible Rust entries (a common shape when the Node /
        // Python / Dart workspaces of a polyglot monorepo are updated in the
        // same `changepacks update` invocation), the per-package guard
        // (`package.language() != Language::Rust { continue }`) below would
        // drop every entry, `any_updated` would stay `false`, and the write
        // branch would already never run — but the `read_to_string` + full
        // `DocumentMut` parse would have already happened. Mirrors the
        // "no scheduled work → skip" shape `apply_update_on_rules` and
        // `apply_reverse_dependencies` already use in `changepacks-utils`.
        if !packages
            .iter()
            .any(|p| p.language() == Language::Rust && p.name().is_some())
        {
            return Ok(());
        }
        let cargo_toml_raw = read_to_string(&self.path).await?;
        let mut cargo_toml: DocumentMut = cargo_toml_raw.parse::<DocumentMut>()?;

        // check has workspace.dependencies section
        if cargo_toml.get("workspace").is_none()
            || cargo_toml["workspace"].get("dependencies").is_none()
        {
            return Ok(());
        }
        let dependencies = cargo_toml
            .get_mut("workspace")
            .and_then(|w| w.get_mut("dependencies"))
            .and_then(|d| d.as_table_mut())
            .context("Dependencies section not found")?;

        let mut any_updated = false;
        for package in packages {
            if package.language() != Language::Rust {
                continue;
            }
            let Some(package_name) = package.name() else {
                continue;
            };
            // Single lookup + type check via `get_mut(..).and_then(as_inline_table_mut)`:
            // the previous `.get(k).is_none()` guard + `dependencies[k]` index
            // did the same work in two steps and carried a panic surface on
            // `[]` indexing. `let-else` continues on either a missing key or
            // a non-inline-table value — byte-identical semantics.
            let Some(dep) = dependencies
                .get_mut(package_name)
                .and_then(toml_edit::Item::as_inline_table_mut)
            else {
                continue;
            };
            if let Some(current_version) = dep.get("version").and_then(|v| v.as_str())
                && let Some(next_version) = package.version()
            {
                // `split_version` is now total (no `Result`), so the `?`
                // that used to propagate its never-fires `Err` variant is
                // gone. Semantics stay byte-identical for every input.
                let (prefix, _) = split_version(current_version);
                dep["version"] = format!("{}{}", prefix.unwrap_or_default(), next_version).into();
                any_updated = true;
            }
        }

        if !any_updated {
            return Ok(());
        }

        write(
            &self.path,
            finalize_content(&cargo_toml.to_string(), &cargo_toml_raw),
        )
        .await?;

        Ok(())
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
    async fn test_rust_workspace_new() {
        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(workspace.name(), Some("test-workspace"));
        assert_eq!(workspace.version(), Some("1.0.0"));
        assert_eq!(workspace.path(), PathBuf::from("/test/Cargo.toml"));
        assert_eq!(workspace.relative_path(), PathBuf::from("test/Cargo.toml"));
        assert_eq!(workspace.language(), Language::Rust);
        assert!(!workspace.is_changed());
        assert_eq!(
            workspace.default_publish_command(),
            "cargo publish --workspace"
        );
        assert_eq!(
            workspace.default_dry_run_publish_command().as_deref(),
            Some("cargo publish --workspace --dry-run")
        );
    }

    #[tokio::test]
    async fn test_rust_workspace_new_without_name_and_version() {
        let workspace = RustWorkspace::new(
            None,
            None,
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
    }

    #[tokio::test]
    async fn test_rust_workspace_set_changed() {
        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
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
    async fn test_rust_workspace_update_version_with_existing_package(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[package]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(update_type).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains(&format!("version = \"{expected}\"")));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_without_package_section() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            None,
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("[package]"));
        assert!(content.contains("version = \"0.0.1\""));
        assert!(content.contains("name = \"test-workspace\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_without_name() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]
"#,
        )
        .unwrap();

        let mut workspace =
            RustWorkspace::new(None, None, cargo_toml.clone(), PathBuf::from("Cargo.toml"));

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("[package]"));
        assert!(content.contains("version = \"0.0.1\""));
        assert!(content.contains("name = \"_\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_preserves_existing_name() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[package]
name = "existing-name"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("name = \"existing-name\""));
        assert!(content.contains("version = \"1.0.1\""));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_rust_workspace_dependencies() {
        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
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

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
core = { version = "1.0.0", path = "crates/core" }
utils = { version = "2.0.0", path = "crates/utils" }
"#,
        )
        .unwrap();

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        // Create mock packages with updated versions
        let mut core_pkg = RustPackage::new(
            Some("core".to_string()),
            Some("1.1.0".to_string()),
            PathBuf::from("/test/crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );
        core_pkg.set_changed(true);

        let packages: Vec<&dyn Package> = vec![&core_pkg];

        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains("version = \"1.1.0\""));
        // utils should remain unchanged
        assert!(content.contains("version = \"2.0.0\""));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies_without_dependency_version() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
core = { path = "crates/core" }
utils = { version = "2.0.0", path = "crates/utils" }
"#,
        )
        .unwrap();

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        let core_pkg = RustPackage::new(
            Some("core".to_string()),
            Some("1.1.0".to_string()),
            PathBuf::from("/test/crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );

        let packages: Vec<&dyn Package> = vec![&core_pkg];

        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains(r#"core = { path = "crates/core" }"#));
        assert!(content.contains(r#"utils = { version = "2.0.0", path = "crates/utils" }"#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies_without_package_version() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
core = { version = "1.0.0", path = "crates/core" }
"#,
        )
        .unwrap();

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        let core_pkg = RustPackage::new(
            Some("core".to_string()),
            None,
            PathBuf::from("/test/crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );

        let packages: Vec<&dyn Package> = vec![&core_pkg];

        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert!(content.contains(r#"core = { version = "1.0.0", path = "crates/core" }"#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies_no_workspace_section() {
        use crate::package::RustPackage;

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

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        let core_pkg = RustPackage::new(
            Some("core".to_string()),
            Some("1.1.0".to_string()),
            PathBuf::from("/test/crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );

        let packages: Vec<&dyn Package> = vec![&core_pkg];

        // Should complete without error even without workspace.dependencies
        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_updates_workspace_package_version() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "1.0.0"
edition = "2024"

[package]
name = "test-workspace"
version = "1.0.0"
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        // Both should be updated
        assert!(content.contains(r#"version = "1.1.0""#));
        // workspace.package.version should also be updated
        // Parse to verify both locations
        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(doc["package"]["version"].as_str(), Some("1.1.0"));
        assert_eq!(
            doc["workspace"]["package"]["version"].as_str(),
            Some("1.1.0")
        );
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_virtual_workspace() {
        // Virtual workspace: has [workspace.package].version but NO [package] section
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.33"
edition = "2024"
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            None,
            Some("0.1.33".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        // [workspace.package].version should be updated
        assert_eq!(
            doc["workspace"]["package"]["version"].as_str(),
            Some("0.1.34")
        );
        // [package] section should NOT be created for virtual workspaces
        assert!(
            doc.get("package").is_none(),
            "virtual workspace should not get a [package] section"
        );
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_syncs_workspace_dependencies() {
        // Virtual workspace with [workspace.dependencies] containing path deps
        // that share the workspace version — these should be synced on bump
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.33"
edition = "2024"

[workspace.dependencies]
vespera_core = { path = "crates/vespera_core", version = "0.1.33" }
vespera_macro = { path = "crates/vespera_macro", version = "0.1.33" }
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1.0", features = ["full"] }
other_local = { path = "crates/other", version = "0.5.0" }
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            None,
            Some("0.1.33".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        let doc: toml_edit::DocumentMut = content.parse().unwrap();

        // [workspace.package].version bumped
        assert_eq!(
            doc["workspace"]["package"]["version"].as_str(),
            Some("0.1.34")
        );

        // Path deps matching old version should be synced
        let ws_deps = doc["workspace"]["dependencies"].as_table().unwrap();
        assert_eq!(
            ws_deps["vespera_core"]["version"].as_str(),
            Some("0.1.34"),
            "path dep with matching version should be bumped"
        );
        assert_eq!(
            ws_deps["vespera_macro"]["version"].as_str(),
            Some("0.1.34"),
            "path dep with matching version should be bumped"
        );

        // Non-path deps should NOT be touched
        assert_eq!(
            ws_deps["serde"]["version"].as_str(),
            Some("1.0"),
            "non-path dep should remain unchanged"
        );
        assert_eq!(
            ws_deps["tokio"]["version"].as_str(),
            Some("1.0"),
            "non-path dep should remain unchanged"
        );

        // Path dep with different version should NOT be touched
        assert_eq!(
            ws_deps["other_local"]["version"].as_str(),
            Some("0.5.0"),
            "path dep with different version should remain unchanged"
        );

        // No [package] section created
        assert!(doc.get("package").is_none());
    }

    #[test]
    fn test_set_name() {
        let mut workspace = RustWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("Cargo.toml"),
        );
        assert_eq!(workspace.name(), None);
        workspace.set_name("my-project".to_string());
        assert_eq!(workspace.name(), Some("my-project"));
    }
}
