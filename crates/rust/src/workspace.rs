use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType, Workspace};
use changepacks_utils::{
    next_version, replace_version_keep_prefix, split_version, write_finalized,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

#[derive(Clone, Debug, Default)]
pub(crate) struct InheritedWorkspaceMemberIdentities {
    package_names: HashSet<String>,
    dependency_aliases: HashSet<String>,
}

impl InheritedWorkspaceMemberIdentities {
    pub(crate) fn record(&mut self, package_name: &str, aliases: impl IntoIterator<Item = String>) {
        self.package_names.insert(package_name.to_string());
        self.dependency_aliases.extend(aliases);
    }

    fn contains_dependency(&self, dependency_key: &str, package_name: &str) -> bool {
        self.package_names.contains(package_name)
            && (dependency_key == package_name || self.dependency_aliases.contains(dependency_key))
    }
}

pub(crate) type InheritedWorkspaceMembers = Arc<Mutex<InheritedWorkspaceMemberIdentities>>;

/// Lock the shared inherited-member identities, recovering from poisoning.
///
/// The map only ever accumulates member names/aliases, so a panic while a
/// writer held the guard cannot leave it logically inconsistent — taking the
/// inner value keeps discovery working instead of cascading the panic. This is
/// the single definition of that policy: every lock site in this crate goes
/// through here, so the recovery strategy changes in exactly one place.
///
/// A free function rather than an inherent method because
/// `InheritedWorkspaceMembers` is an alias for `Arc<Mutex<_>>`, and inherent
/// impls are not allowed on types from another crate.
pub(crate) fn lock_recovering(
    members: &InheritedWorkspaceMembers,
) -> MutexGuard<'_, InheritedWorkspaceMemberIdentities> {
    members.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
pub struct RustWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
    dependencies: HashSet<String>,
    publishable_by_default: bool,
    inherited_workspace_members: InheritedWorkspaceMembers,
}

impl RustWorkspace {
    /// Construct a Rust workspace without discovered inherited-member metadata.
    ///
    /// Version updates still update the workspace/root package version, but do
    /// not perform the special inherited-member fan-out in
    /// `[workspace.dependencies]`. `RustProjectFinder` uses the internal
    /// member-aware constructor after discovery.
    #[must_use]
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self::new_with_inherited_workspace_members(
            name,
            version,
            path,
            relative_path,
            InheritedWorkspaceMembers::default(),
            true,
        )
    }

    #[must_use]
    pub(crate) fn new_with_inherited_workspace_members(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        inherited_workspace_members: InheritedWorkspaceMembers,
        package_publishable_by_default: bool,
    ) -> Self {
        let publishable_by_default = name.is_some() && package_publishable_by_default;
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
            dependencies: HashSet::new(),
            publishable_by_default,
            inherited_workspace_members,
        }
    }
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

        if cargo_toml
            .get("package")
            .is_some_and(|package| !package.is_table_like())
        {
            anyhow::bail!(
                "Cargo.toml {} has a non-table [package] item",
                self.path.display()
            );
        }
        let has_package = cargo_toml.get("package").is_some();

        if has_package {
            // A hybrid workspace root can inherit its OWN version via
            // `[package] version.workspace = true` (which toml_edit parses as a
            // `{ workspace = true }` table) alongside `[workspace.package].version`.
            // Rewriting `[package].version` with a plain string here would clobber
            // that inheritance marker — the same file-format break already guarded
            // against for member crates in `RustPackage::update_version`. Detect the
            // marker with the shared `crate::is_workspace_marker` decoder (the same
            // one `finder.rs` uses for `[dependencies]`/`[package]` inheritance);
            // when present, skip the `[package].version` write (the
            // `[workspace.package].version` sync below drives the inherited bump).
            let inherits_workspace_version = cargo_toml["package"]
                .get("version")
                .is_some_and(crate::is_workspace_marker);
            if !inherits_workspace_version {
                cargo_toml["package"]["version"] = new_version.as_str().into();
            }
            if cargo_toml["package"].get("name").is_none() {
                let fallback_name = self.name.as_deref().unwrap_or("_");
                cargo_toml["package"]["name"] = fallback_name.into();
            }
        } else if cargo_toml.get("workspace").is_some() {
            // A manifest with [workspace] but no [package] is virtual even when
            // it has not opted into workspace package metadata yet.
            cargo_toml["workspace"]["package"]["version"] = new_version.as_str().into();
        } else {
            let fallback_name = self.name.as_deref().unwrap_or("_");
            cargo_toml["package"] = toml_edit::Item::Table(toml_edit::Table::new());
            cargo_toml["package"]["version"] = new_version.as_str().into();
            cargo_toml["package"]["name"] = fallback_name.into();
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
        // is missing; virtual workspaces add it in the branch above before
        // reaching this sync.
        if let Some(ws_pkg_version) = cargo_toml
            .get_mut("workspace")
            .and_then(|w| w.get_mut("package"))
            .and_then(|p| p.get_mut("version"))
        {
            *ws_pkg_version = toml_edit::value(new_version.as_str());
        }

        // Sync [workspace.dependencies] only for discovered members that inherit
        // the workspace package version and whose local dependency version matched
        // the old workspace version.
        if let Some(ws_deps) = crate::workspace_dependencies_table_mut(&mut cargo_toml) {
            // Hold the lock GUARD instead of deep-cloning the two
            // `HashSet<String>` behind it: the clone used to run on every
            // workspace bump even when this `if let` did not fire. Taken
            // inside the block so the guard is dropped at its closing brace,
            // strictly before the `.await` below — that keeps this future
            // `Send` for the N-API and PyO3 bridges. Auto-deref makes
            // `contains_dependency` read unchanged through the guard.
            //
            // `old_version` is hoisted to the top of this function so both the
            // `next_version` fallback and this workspace-deps sync share the
            // same "reserve 0.0.0 when unversioned" source.
            let inherited_workspace_members = lock_recovering(&self.inherited_workspace_members);
            for (dependency_key, value) in ws_deps.iter_mut() {
                let dependency_key = dependency_key.get();
                let package_name = crate::finder::effective_dependency_name(dependency_key, value);
                if !inherited_workspace_members.contains_dependency(dependency_key, package_name) {
                    continue;
                }
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

        write_finalized(
            &self.path,
            cargo_toml.to_string(),
            &cargo_toml_raw,
            "Cargo.toml",
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

    // Publishability flag accessor.
    changepacks_core::impl_publishable_by_default!();

    // `default_publish_command` / `default_dry_run_publish_command` share
    // their const-based shape with every other const-driven language
    // crate. Consolidated via `impl_const_publish_commands!()` in
    // `changepacks-core` — expansion is byte-identical to the previous
    // hand-rolled bodies. Hybrid roots publish only their own `[package]`;
    // discovered member projects remain separate topologically sorted
    // publish operations.
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
            .any(|package| package.language() == Language::Rust && package.name().is_some())
        {
            return Ok(());
        }
        let (cargo_toml_raw, mut cargo_toml) = crate::read_and_parse_cargo_toml(&self.path).await?;

        // check has workspace.dependencies section
        //
        // Single-lookup `let-else` over the shared
        // `crate::workspace_dependencies_table_mut` chain (the same helper
        // `update_version` above uses): previously we did TWO independent
        // `get("workspace")` walks (one guard, one grab) plus a
        // `.context("Dependencies section not found")?` that could never fire
        // because the guard above already ensured the chain resolved. The
        // refutable helper returns `None` on any missing hop (no workspace, no
        // dependencies, non-table), so this returns `Ok(())` there —
        // byte-identical semantics with one fewer HashMap-style lookup per
        // invocation.
        let Some(dependencies) = crate::workspace_dependencies_table_mut(&mut cargo_toml) else {
            return Ok(());
        };

        let package_versions: HashMap<&str, &str> = packages
            .iter()
            .filter(|package| package.language() == Language::Rust)
            .filter_map(|package| Some((package.name()?, package.version()?)))
            .collect();
        let mut any_updated = false;
        for (dependency_key, dependency) in dependencies.iter_mut() {
            let package_name =
                crate::finder::effective_dependency_name(dependency_key.get(), dependency);
            let Some(&next_version) = package_versions.get(package_name) else {
                continue;
            };
            // `as_table_like_mut` matches BOTH inline-table deps
            // (`foo = { version = "..." }`) AND sub-table deps
            // (`[workspace.dependencies.foo]`), while string deps still yield
            // `None` and are skipped.
            let Some(dep) = dependency.as_table_like_mut() else {
                continue;
            };
            if dep.get("path").is_some()
                && let Some(current_version) = dep.get("version").and_then(|v| v.as_str())
            {
                // `current_version` borrows `dep`; delegate the prefix-
                // preserving rebuild to the shared `replace_version_keep_prefix`
                // and build the owned bumped string BEFORE taking the
                // `get_mut("version")` mutable borrow so no shared borrow of
                // `dep` outlives it. `TableLike` exposes no `Index`/`[]`
                // operator, so rewrite the value in place via `get_mut` — never
                // insert a `version` key where none exists.
                let bumped = replace_version_keep_prefix(current_version, next_version);
                if bumped == current_version {
                    continue;
                }
                if let Some(v) = dep.get_mut("version") {
                    *v = toml_edit::value(bumped);
                    any_updated = true;
                }
            }
        }

        if !any_updated {
            return Ok(());
        }

        write_finalized(
            &self.path,
            cargo_toml.to_string(),
            &cargo_toml_raw,
            "Cargo.toml",
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

    fn inherited_members(names: &[&str]) -> InheritedWorkspaceMembers {
        let members = InheritedWorkspaceMembers::default();
        {
            let mut identities = lock_recovering(&members);
            for name in names {
                identities.record(name, std::iter::empty());
            }
        }
        members
    }

    #[tokio::test]
    async fn test_rust_workspace_new_hybrid_root_uses_root_scoped_publish_commands() {
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
        assert!(workspace.is_publishable_by_default());
        assert_eq!(workspace.default_publish_command(), "cargo publish");
        assert_eq!(
            workspace.default_dry_run_publish_command().as_deref(),
            Some("cargo publish --dry-run")
        );
    }

    #[tokio::test]
    async fn test_rust_workspace_new_without_name_and_version() {
        let mut workspace = RustWorkspace::new(
            None,
            None,
            PathBuf::from("/test/Cargo.toml"),
            PathBuf::from("test/Cargo.toml"),
        );

        assert_eq!(workspace.name(), None);
        assert_eq!(workspace.version(), None);
        assert!(!workspace.is_publishable_by_default());

        // Project discovery assigns a repository-name fallback to unnamed
        // projects after finder finalization. That display-name mutation must
        // not turn a virtual Cargo workspace into a publishable hybrid root.
        workspace.set_name("repository-name".to_string());
        assert_eq!(workspace.name(), Some("repository-name"));
        assert!(!workspace.is_publishable_by_default());
    }

    #[tokio::test]
    async fn test_rust_workspace_new_has_no_inherited_member_fanout_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.package]
version = "1.0.0"

[workspace.dependencies]
same-version-local = { path = "crates/same-version-local", version = "1.0.0" }
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            None,
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );
        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        let document: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(
            document["workspace"]["package"]["version"].as_str(),
            Some("1.0.1")
        );
        assert_eq!(
            document["workspace"]["dependencies"]["same-version-local"]["version"].as_str(),
            Some("1.0.0")
        );
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
    async fn test_rust_workspace_update_version_non_table_package_error_includes_path() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, "package = 3\n").unwrap();

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        let err = workspace
            .update_version(UpdateType::Minor)
            .await
            .expect_err("non-table package item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains(&cargo_toml.display().to_string()),
            "error chain should name the manifest path, got: {chain}"
        );
        assert!(
            chain.contains("non-table [package]"),
            "error chain should mention the non-table package item, got: {chain}"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_non_table_package_leaves_state_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        // A scalar top-level `package` key. The sibling test above pins the
        // ERROR TEXT; this one pins the guard's actual reason for existing — it
        // must reject BEFORE the `cargo_toml["package"]["version"] = ...`
        // assignment reaches `write_finalized` AND before `self.version` is
        // advanced, so neither the manifest on disk nor the in-memory version
        // moves. Mirrors the package-side
        // `test_write_cargo_package_version_non_table_package_leaves_file_untouched`
        // in `lib.rs`.
        let original = "package = \"not-a-table\"\n\n[workspace]\nmembers = [\"crates/*\"]\n";
        fs::write(&cargo_toml, original).unwrap();

        let mut workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        let err = workspace
            .update_version(UpdateType::Patch)
            .await
            .expect_err("non-table package item must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("has a non-table [package] item"),
            "error chain should name the non-table package guard, got: {chain}"
        );

        // Byte-for-byte, not line-for-line: a partial or reformatted write is
        // exactly the manifest destruction the guard prevents.
        assert_eq!(
            fs::read(&cargo_toml).unwrap(),
            original.as_bytes(),
            "a rejected bump must leave the manifest byte-identical"
        );
        assert_eq!(
            workspace.version(),
            Some("1.0.0"),
            "a rejected bump must not advance the in-memory version"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_version_without_package_section() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(&cargo_toml, "").unwrap();

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
        fs::write(&cargo_toml, "").unwrap();

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
serde = { version = "1.0.0" }
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

        let mut serde_pkg = RustPackage::new(
            Some("serde".to_string()),
            Some("1.1.0".to_string()),
            PathBuf::from("/test/serde/Cargo.toml"),
            PathBuf::from("serde/Cargo.toml"),
        );
        serde_pkg.set_changed(true);

        let packages: Vec<&dyn Package> = vec![&core_pkg, &serde_pkg];

        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert_eq!(
            content,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
core = { version = "1.1.0", path = "crates/core" }
utils = { version = "2.0.0", path = "crates/utils" }
serde = { version = "1.0.0" }
"#
        );

        temp_dir.close().unwrap();
    }

    /// Fixture shared by the two fast-path guard tests below: writes a
    /// `[workspace.dependencies]` manifest whose entries carry BOTH `path` and
    /// `version` keys, i.e. exactly the shape the update loop would rewrite if
    /// it ever ran, and returns it together with its raw bytes.
    fn write_path_dep_workspace_manifest(dir: &TempDir) -> (PathBuf, Vec<u8>) {
        let cargo_toml = dir.path().join("Cargo.toml");
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
        let bytes = fs::read(&cargo_toml).unwrap();
        (cargo_toml, bytes)
    }

    /// Asserts the two observable effects of the fast-path guard in
    /// `update_workspace_dependencies` for a `packages` slice holding zero
    /// eligible Rust entries: the manifest is left byte-identical (no write),
    /// and a workspace whose manifest does not exist on disk still returns
    /// `Ok(())` (no read/parse at all — removing the guard turns this second
    /// case into the `read_and_parse_cargo_toml` error).
    async fn assert_workspace_dependencies_fast_path(packages: &[&dyn Package]) {
        let temp_dir = TempDir::new().unwrap();
        let (cargo_toml, before) = write_path_dep_workspace_manifest(&temp_dir);

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );
        workspace
            .update_workspace_dependencies(packages)
            .await
            .expect("fast path must succeed");
        assert_eq!(
            fs::read(&cargo_toml).unwrap(),
            before,
            "fast path must leave the manifest byte-identical"
        );

        // No manifest on disk: only the guard's early return can keep this
        // `Ok(())`, so this pins "the fast path never reads or parses".
        let missing = temp_dir.path().join("missing").join("Cargo.toml");
        let absent_workspace = RustWorkspace::new(
            Some("absent-workspace".to_string()),
            Some("1.0.0".to_string()),
            missing.clone(),
            PathBuf::from("missing/Cargo.toml"),
        );
        absent_workspace
            .update_workspace_dependencies(packages)
            .await
            .expect("fast path must not touch the filesystem");
        assert!(
            !missing.exists(),
            "fast path must not create the missing manifest"
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies_empty_packages() {
        assert_workspace_dependencies_fast_path(&[]).await;
    }

    #[tokio::test]
    async fn test_rust_workspace_update_workspace_dependencies_no_rust_packages() {
        let node_pkg = changepacks_core::test_support::MockPackage::with_all(
            Some("core"),
            Some("9.9.9"),
            "/test/packages/core/package.json",
            "packages/core/package.json",
            Language::Node,
        );
        let packages: Vec<&dyn Package> = vec![&node_pkg];

        assert_workspace_dependencies_fast_path(&packages).await;
    }

    #[tokio::test]
    async fn test_rust_workspace_updates_inline_and_table_form_dependency_aliases() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
renamed-core = { package = "core", version = "1.0.0", path = "crates/core" }

[workspace.dependencies.renamed-utils]
package = "utils"
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
            temp_dir.path().join("crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );
        let utils_pkg = RustPackage::new(
            Some("utils".to_string()),
            Some("2.1.0".to_string()),
            temp_dir.path().join("crates/utils/Cargo.toml"),
            PathBuf::from("crates/utils/Cargo.toml"),
        );
        let packages: Vec<&dyn Package> = vec![&core_pkg, &utils_pkg];

        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert_eq!(
            content,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies]
renamed-core = { package = "core", version = "1.1.0", path = "crates/core" }

[workspace.dependencies.renamed-utils]
package = "utils"
version = "2.1.0"
path = "crates/utils"
"#
        );

        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_skips_current_inline_dependency_write() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        let original = r#"[workspace]
[workspace.dependencies]
core = { version = "~1.2.3", path = "crates/core" }
"#;
        fs::write(&cargo_toml, original).unwrap();
        let writable_permissions = fs::metadata(&cargo_toml).unwrap().permissions();
        let mut readonly_permissions = writable_permissions.clone();
        readonly_permissions.set_readonly(true);
        fs::set_permissions(&cargo_toml, readonly_permissions).unwrap();

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );
        let core_pkg = RustPackage::new(
            Some("core".to_string()),
            Some("1.2.3".to_string()),
            temp_dir.path().join("crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );

        workspace
            .update_workspace_dependencies(&[&core_pkg])
            .await
            .unwrap();

        assert_eq!(fs::read(&cargo_toml).unwrap(), original.as_bytes());
        fs::set_permissions(&cargo_toml, writable_permissions).unwrap();
        temp_dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_rust_workspace_skips_current_subtable_dependency_write() {
        use crate::package::RustPackage;

        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        let original = r#"[workspace]
[workspace.dependencies.core]
version = "^2.0.0"
path = "crates/core"
"#;
        fs::write(&cargo_toml, original).unwrap();
        let writable_permissions = fs::metadata(&cargo_toml).unwrap().permissions();
        let mut readonly_permissions = writable_permissions.clone();
        readonly_permissions.set_readonly(true);
        fs::set_permissions(&cargo_toml, readonly_permissions).unwrap();

        let workspace = RustWorkspace::new(
            Some("test-workspace".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );
        let core_pkg = RustPackage::new(
            Some("core".to_string()),
            Some("2.0.0".to_string()),
            temp_dir.path().join("crates/core/Cargo.toml"),
            PathBuf::from("crates/core/Cargo.toml"),
        );

        workspace
            .update_workspace_dependencies(&[&core_pkg])
            .await
            .unwrap();

        assert_eq!(fs::read(&cargo_toml).unwrap(), original.as_bytes());
        fs::set_permissions(&cargo_toml, writable_permissions).unwrap();
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
    async fn test_rust_workspace_update_version_preserves_hybrid_root_inheritance_marker() {
        // Regression: a hybrid workspace root that is BOTH the workspace and a
        // publishable crate inheriting its own version
        // (`[package] version.workspace = true` alongside
        // `[workspace.package].version`). `update_version` used to
        // unconditionally rewrite `[package].version` with a plain string,
        // clobbering the dotted-key inheritance marker and permanently
        // detaching the root crate from the workspace-wide bump. The correct
        // behavior leaves the marker intact and drives the bump through
        // `[workspace.package].version` only — the same guarantee already held
        // for member crates in `RustPackage::update_version`.
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
name = "root"
version.workspace = true
"#,
        )
        .unwrap();

        let mut workspace = RustWorkspace::new(
            Some("root".to_string()),
            Some("1.0.0".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
        );

        workspace.update_version(UpdateType::Minor).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        // The dotted-key inheritance marker survives verbatim — not rewritten
        // to a hardcoded `version = "..."`.
        assert!(
            content.contains("version.workspace = true"),
            "inheritance marker was clobbered: {content}"
        );

        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        // [package].version is still the marker table, NOT a string literal.
        assert!(
            doc["package"]["version"].as_str().is_none(),
            "[package].version was rewritten to a string literal: {content}"
        );
        assert_eq!(
            doc["package"]["version"]["workspace"].as_bool(),
            Some(true),
            "[package].version should remain `workspace = true`"
        );
        // The inherited bump is driven through [workspace.package].version.
        assert_eq!(
            doc["workspace"]["package"]["version"].as_str(),
            Some("1.1.0")
        );

        temp_dir.close().unwrap();
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
    async fn test_rust_workspace_update_version_virtual_workspace_without_version() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"[workspace]
resolver = "2"
members = ["crates/*"]
"#,
        )
        .unwrap();

        let mut workspace =
            RustWorkspace::new(None, None, cargo_toml.clone(), PathBuf::from("Cargo.toml"));

        workspace.update_version(UpdateType::Patch).await.unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        let doc: toml_edit::DocumentMut = content.parse().unwrap();
        assert_eq!(
            doc["workspace"]["package"]["version"].as_str(),
            Some("0.0.1")
        );
        assert!(
            doc.get("package").is_none(),
            "virtual workspace should not get a [package] section: {content}"
        );
        assert!(
            !content.contains("_"),
            "virtual workspace should not get a placeholder name: {content}"
        );

        temp_dir.close().unwrap();
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

        let mut workspace = RustWorkspace::new_with_inherited_workspace_members(
            None,
            Some("0.1.33".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
            inherited_members(&["vespera_core", "vespera_macro"]),
            true,
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

        let mut workspace = RustWorkspace::new_with_inherited_workspace_members(
            None,
            Some("0.1.33".to_string()),
            cargo_toml.clone(),
            PathBuf::from("Cargo.toml"),
            inherited_members(&["vespera_core", "vespera_macro"]),
            true,
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

[workspace.dependencies.serde]
version = "1.0.0"
features = ["derive"]
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

        let mut serde_pkg = RustPackage::new(
            Some("serde".to_string()),
            Some("1.1.0".to_string()),
            PathBuf::from("/test/serde/Cargo.toml"),
            PathBuf::from("serde/Cargo.toml"),
        );
        serde_pkg.set_changed(true);

        let packages: Vec<&dyn Package> = vec![&core_pkg, &serde_pkg];

        workspace
            .update_workspace_dependencies(&packages)
            .await
            .unwrap();

        let content = read_to_string(&cargo_toml).await.unwrap();
        assert_eq!(
            content,
            r#"[workspace]
members = ["crates/*"]

[workspace.dependencies.core]
version = "1.1.0"
path = "crates/core"

[workspace.dependencies.utils]
version = "2.0.0"
path = "crates/utils"

[workspace.dependencies.serde]
version = "1.0.0"
features = ["derive"]
"#
        );

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
