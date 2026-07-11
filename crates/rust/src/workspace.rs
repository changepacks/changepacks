use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType, Workspace};
use changepacks_utils::{
    finalize_content, next_version, replace_version_keep_prefix, split_version,
};
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::fs::write;

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
        // `next_version` here AND for the `[workspace.dependencies]` sync
        // at the loop below. Since the fallback is already applied inline,
        // `next_version` is byte-identical to (and simpler than) the
        // previous `next_version_or_default(Some(old_version), ...)`
        // indirection — `_or_default`'s whole purpose is to fold that
        // fallback, pointless once the fallback has already fired.
        let old_version = self.version.as_deref().unwrap_or("0.0.0");
        let new_version = next_version(old_version, update_type)?;

        let (cargo_toml_raw, mut cargo_toml) = crate::read_and_parse_cargo_toml(&self.path).await?;

        let has_package = cargo_toml.get("package").is_some();

        if has_package {
            cargo_toml["package"]["version"] = new_version.as_str().into();
            if cargo_toml["package"].get("name").is_none() {
                let fallback_name = self.name.as_deref().unwrap_or("_");
                cargo_toml["package"]["name"] = fallback_name.into();
            }
        } else {
            // No [package] section: the 3-hop `[workspace.package].version`
            // walk is now computed HERE instead of unconditionally at the
            // top of the function. On the dominant "has [package]" shape
            // (typical single-crate + workspace root — the shape used by
            // the changepacks repo itself), the has_package arm above never
            // reads the workspace-package-version answer, so hoisting it
            // into this branch skips the
            // `get(..).and_then(..).and_then(..).is_some()` trip on every
            // has_package invocation. Byte-identical semantics: the else
            // branch is still guarded on "no [package] AND no
            // [workspace.package].version".
            let has_workspace_package_version = cargo_toml
                .get("workspace")
                .and_then(|w| w.get("package"))
                .and_then(|p| p.get("version"))
                .is_some();
            if !has_workspace_package_version {
                // No [package] and no [workspace.package].version — create [package]
                let fallback_name = self.name.as_deref().unwrap_or("_");
                cargo_toml["package"] = toml_edit::Item::Table(toml_edit::Table::new());
                cargo_toml["package"]["version"] = new_version.as_str().into();
                cargo_toml["package"]["name"] = fallback_name.into();
            }
            // else: virtual workspace — only [workspace.package].version needs updating
            // (below); `fallback_name` is not computed since [package] is untouched here.
        }

        // Update [workspace.package].version if it exists.
        //
        // Single-lookup `get_mut` chain: mirrors the "single lookup + type
        // check via `get_mut(..)`" idiom already used by
        // `update_workspace_dependencies` below (see the comment near
        // `dependencies.get_mut(package_name).and_then(as_inline_table_mut)`).
        // The previous shape called `as_table_mut()` + `contains_key("version")`
        // and then re-indexed `ws_pkg["version"] = ...`, doing the HashMap-style
        // key walk twice and carrying a latent panic surface behind the
        // `contains_key` gate. `get_mut("version")` returns `None` when the key
        // is missing, so no `version` key is added to a `[workspace.package]`
        // table that lacks one — byte-identical to the previous behavior
        // (test_rust_workspace_update_version_virtual_workspace fixes the
        // has-version path; the without-version path stays a no-op just as
        // before).
        if let Some(ws_pkg_version) = cargo_toml
            .get_mut("workspace")
            .and_then(|w| w.get_mut("package"))
            .and_then(|p| p.get_mut("version"))
        {
            *ws_pkg_version = toml_edit::value(new_version.as_str());
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
                // gates that still filter the branch (`as_table_like_mut`,
                // `dep.get("path").is_some()`, `dep.get("version").as_str()`)
                // stay in the `&&`-chain unchanged. `as_table_like_mut` accepts
                // BOTH inline-table deps (`foo = { path = "...", version = "..."
                // }`) AND sub-table deps (`[workspace.dependencies.foo]` with
                // `path`/`version` keys), bringing the writer into parity with
                // the reader's `as_table_like()` in `finder.rs`. String deps
                // (`foo = "1.0"`) still yield `None`, so they remain skipped.
                if let Some(dep) = value.as_table_like_mut()
                    && dep.get("path").is_some()
                    && let Some(ver_str) = dep.get("version").and_then(|v| v.as_str())
                {
                    // Only the numeric tail (`split_version`'s `.1`) gates the
                    // sync; the prefix-preserving rebuild is delegated to the
                    // shared `replace_version_keep_prefix` so the "keep prefix,
                    // swap version" policy lives next to `split_version`.
                    if split_version(ver_str).1 == old_version {
                        // `ver_str` borrows `dep`; build the owned bumped string
                        // BEFORE taking the `get_mut("version")` mutable borrow
                        // so no shared borrow of `dep` outlives it. `TableLike`
                        // exposes no `Index`/`[]` operator, so rewrite the value
                        // in place via `get_mut` — never insert a `version` key
                        // where none exists (the guard above already proved it
                        // does).
                        let bumped = replace_version_keep_prefix(ver_str, &new_version);
                        if let Some(v) = dep.get_mut("version") {
                            *v = toml_edit::value(bumped);
                        }
                    }
                }
            }
        }

        write(
            &self.path,
            finalize_content(&cargo_toml.to_string(), &cargo_toml_raw),
        )
        .await
        .with_context(|| format!("Failed to write Cargo.toml {}", self.path.display()))?;
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
        let (cargo_toml_raw, mut cargo_toml) = crate::read_and_parse_cargo_toml(&self.path).await?;

        // check has workspace.dependencies section
        //
        // Single-lookup `let-else` over the `get_mut` chain: previously we
        // did TWO independent `get("workspace")` walks (one guard, one grab)
        // plus a `.context("Dependencies section not found")?` that could
        // never fire because the guard above already ensured the chain
        // resolved. The refutable chain here returns `Ok(())` on any missing
        // hop (no workspace, no dependencies, non-table) — byte-identical
        // semantics with one fewer HashMap-style lookup per invocation.
        let Some(dependencies) = cargo_toml
            .get_mut("workspace")
            .and_then(|w| w.get_mut("dependencies"))
            .and_then(|d| d.as_table_mut())
        else {
            return Ok(());
        };

        let mut any_updated = false;
        for package in packages {
            if package.language() != Language::Rust {
                continue;
            }
            let Some(package_name) = package.name() else {
                continue;
            };
            // Single lookup + type check via `get_mut(..).and_then(as_table_like_mut)`:
            // the previous `.get(k).is_none()` guard + `dependencies[k]` index
            // did the same work in two steps and carried a panic surface on
            // `[]` indexing. `let-else` continues on either a missing key or
            // a non-table-like value. `as_table_like_mut` matches BOTH inline-
            // table deps (`foo = { version = "..." }`) AND sub-table deps
            // (`[workspace.dependencies.foo]`), while string deps still yield
            // `None` and are skipped.
            let Some(dep) = dependencies
                .get_mut(package_name)
                .and_then(toml_edit::Item::as_table_like_mut)
            else {
                continue;
            };
            if let Some(current_version) = dep.get("version").and_then(|v| v.as_str())
                && let Some(next_version) = package.version()
            {
                // `current_version` borrows `dep`; delegate the prefix-
                // preserving rebuild to the shared `replace_version_keep_prefix`
                // and build the owned bumped string BEFORE taking the
                // `get_mut("version")` mutable borrow so no shared borrow of
                // `dep` outlives it. `TableLike` exposes no `Index`/`[]`
                // operator, so rewrite the value in place via `get_mut` — never
                // insert a `version` key where none exists.
                let bumped = replace_version_keep_prefix(current_version, next_version);
                if let Some(v) = dep.get_mut("version") {
                    *v = toml_edit::value(bumped);
                }
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
        .await
        .with_context(|| format!("Failed to write Cargo.toml {}", self.path.display()))?;

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

    #[tokio::test]
    async fn test_rust_workspace_update_version_syncs_subtable_dependencies() {
        // Same sync as above, but the [workspace.dependencies] entries use the
        // sub-table shape (`[workspace.dependencies.foo]`) instead of inline
        // tables. Path deps sharing the workspace version should be bumped,
        // with sibling keys (`path`, `features`) and the sub-table formatting
        // preserved.
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

[workspace.dependencies.vespera_core]
path = "crates/vespera_core"
version = "0.1.33"

[workspace.dependencies.vespera_macro]
version = "0.1.33"
path = "crates/vespera_macro"
features = ["derive"]

[workspace.dependencies.serde]
version = "1.0"
features = ["derive"]

[workspace.dependencies.other_local]
path = "crates/other"
version = "0.5.0"
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

        let ws_deps = doc["workspace"]["dependencies"].as_table().unwrap();

        // Sub-table path deps matching the old version are bumped
        assert_eq!(
            ws_deps["vespera_core"]["version"].as_str(),
            Some("0.1.34"),
            "sub-table path dep with matching version should be bumped"
        );
        assert_eq!(
            ws_deps["vespera_macro"]["version"].as_str(),
            Some("0.1.34"),
            "sub-table path dep with matching version should be bumped"
        );

        // Sibling keys preserved on the bumped sub-table deps
        assert_eq!(
            ws_deps["vespera_core"]["path"].as_str(),
            Some("crates/vespera_core")
        );
        assert_eq!(
            ws_deps["vespera_macro"]["path"].as_str(),
            Some("crates/vespera_macro")
        );
        assert_eq!(
            ws_deps["vespera_macro"]["features"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "sibling features array should be preserved"
        );

        // Non-path (registry) sub-table dep is untouched even though its
        // version equals the old workspace version.
        assert_eq!(
            ws_deps["serde"]["version"].as_str(),
            Some("1.0"),
            "non-path sub-table dep should remain unchanged"
        );

        // Path dep with a different version is untouched
        assert_eq!(
            ws_deps["other_local"]["version"].as_str(),
            Some("0.5.0"),
            "sub-table path dep with different version should remain unchanged"
        );

        // Sub-table shape + formatting preserved (not rewritten to inline)
        assert!(content.contains("[workspace.dependencies.vespera_core]"));
        assert!(content.contains(r#"path = "crates/vespera_core""#));

        // No [package] section created for a virtual workspace
        assert!(doc.get("package").is_none());

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_subtable_non_matching_untouched() {
        // Sub-table deps that update_version must NOT sync:
        //  - a registry dep (version present but NO `path`)
        //  - a path dep whose version does not match the workspace version
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

[workspace.dependencies.registry_only]
version = "0.1.33"
features = ["derive"]

[workspace.dependencies.mismatched_local]
path = "crates/mismatched"
version = "0.5.0"
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

        assert_eq!(
            doc["workspace"]["package"]["version"].as_str(),
            Some("0.1.34")
        );

        let ws_deps = doc["workspace"]["dependencies"].as_table().unwrap();

        // No `path` → not a workspace member → left at its old version even
        // though it happens to equal the workspace version.
        assert_eq!(
            ws_deps["registry_only"]["version"].as_str(),
            Some("0.1.33"),
            "registry sub-table dep without path should remain unchanged"
        );

        // Has `path` but a different version → not synced.
        assert_eq!(
            ws_deps["mismatched_local"]["version"].as_str(),
            Some("0.5.0"),
            "sub-table path dep with different version should remain unchanged"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies_subtable() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies.core]
version = "1.0.0"
path = "crates/core"

[workspace.dependencies.utils]
version = "2.0.0"
path = "crates/utils"
"#,
        )
        .unwrap();

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

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
        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        let ws_deps = doc["workspace"]["dependencies"].as_table().unwrap();

        // core sub-table dep bumped to the package version
        assert_eq!(ws_deps["core"]["version"].as_str(), Some("1.1.0"));
        // sibling path preserved
        assert_eq!(ws_deps["core"]["path"].as_str(), Some("crates/core"));
        // utils (not in the package set) untouched
        assert_eq!(ws_deps["utils"]["version"].as_str(), Some("2.0.0"));

        // Sub-table shape + formatting preserved
        assert!(content.contains("[workspace.dependencies.core]"));
        assert!(content.contains(r#"path = "crates/core""#));

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies_subtable_without_version() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies.core]
path = "crates/core"

[workspace.dependencies.utils]
version = "2.0.0"
path = "crates/utils"
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
        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        let ws_deps = doc["workspace"]["dependencies"].as_table().unwrap();

        // core has no version key: none should be inserted
        assert!(
            ws_deps["core"].get("version").is_none(),
            "no version key should be inserted into a sub-table dep that lacks one"
        );
        assert_eq!(ws_deps["core"]["path"].as_str(), Some("crates/core"));
        // utils untouched
        assert_eq!(ws_deps["utils"]["version"].as_str(), Some("2.0.0"));

        temp_dir.close().unwrap();
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
