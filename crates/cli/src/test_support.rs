//! Shared `#[cfg(test)]` mocks for this crate's unit tests.
//!
//! Declared `#[cfg(test)] pub(crate) mod test_support;` in `lib.rs`, so nothing
//! here is compiled into a non-test build or shipped in the public API. This
//! consolidates the previously duplicated `MockPackage` / `MockWorkspace`
//! definitions from `commands/check.rs` (formerly `MockPackageForCheck` /
//! `MockWorkspaceForCheck`) and `options/filter_options.rs` behind one surface,
//! mirroring the precedent set by `crates/core/src/test_support.rs`.
//!
//! Both mocks consume the production `changepacks_core::impl_basic_accessors!()`
//! and `changepacks_core::impl_dependencies_accessors!()` macros so the macros'
//! field-name contract is still locked by the type system: rename a struct
//! field (e.g. `is_changed` -> `changed`) and these mocks fail to compile
//! immediately, catching the regression at the CLI-test surface.

use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType, Workspace};

/// Shared test-only publish command defaults for both mocks.
///
/// Kept as a macro so the `"echo publish"` / `"echo publish --dry-run"`
/// contract lives at ONE surface across the `Package` and `Workspace` impls,
/// mirroring how the real impls consume `impl_const_publish_commands!` and how
/// `crates/core/src/test_support.rs` shares its own publish defaults.
macro_rules! impl_test_publish_commands {
    () => {
        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }

        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
        }
    };
}

/// Field name `is_changed` matches the `impl_basic_accessors!()` macro contract
/// (see `crates/core/src/project_finder.rs`) so the shared macro can generate
/// every trivial accessor.
#[derive(Debug)]
pub(crate) struct MockPackage {
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) path: PathBuf,
    pub(crate) relative_path: PathBuf,
    pub(crate) language: Language,
    pub(crate) dependencies: HashSet<String>,
    pub(crate) is_changed: bool,
}

impl MockPackage {
    /// Construct with explicit `name` / `version` / `path` / `relative_path` /
    /// `language`, defaulting `dependencies` to empty and `is_changed` to false.
    pub(crate) fn new(
        name: Option<&str>,
        version: Option<&str>,
        path: &str,
        relative_path: &str,
        language: Language,
    ) -> Self {
        Self {
            name: name.map(String::from),
            version: version.map(String::from),
            path: PathBuf::from(path),
            relative_path: PathBuf::from(relative_path),
            language,
            dependencies: HashSet::new(),
            is_changed: false,
        }
    }
}

#[async_trait]
impl Package for MockPackage {
    // Consumes the same `impl_basic_accessors!()` / `impl_dependencies_accessors!()`
    // macros that every real-world `Package` impl uses. This mock exists to prove
    // the macros' field-name contract survives future edits: rename a struct
    // field and this fails to compile immediately.
    changepacks_core::impl_basic_accessors!();
    changepacks_core::impl_dependencies_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
        Ok(())
    }
    fn language(&self) -> Language {
        self.language
    }
    impl_test_publish_commands!();
}

/// Field name `is_changed` matches the `impl_basic_accessors!()` macro contract
/// (see `MockPackage` above for rationale).
#[derive(Debug)]
pub(crate) struct MockWorkspace {
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) path: PathBuf,
    pub(crate) relative_path: PathBuf,
    pub(crate) language: Language,
    pub(crate) dependencies: HashSet<String>,
    pub(crate) is_changed: bool,
}

impl MockWorkspace {
    /// Construct with explicit `name` / `version` / `path` / `relative_path` /
    /// `language`, defaulting `dependencies` to empty and `is_changed` to false.
    pub(crate) fn new(
        name: Option<&str>,
        version: Option<&str>,
        path: &str,
        relative_path: &str,
        language: Language,
    ) -> Self {
        Self {
            name: name.map(String::from),
            version: version.map(String::from),
            path: PathBuf::from(path),
            relative_path: PathBuf::from(relative_path),
            language,
            dependencies: HashSet::new(),
            is_changed: false,
        }
    }
}

#[async_trait]
impl Workspace for MockWorkspace {
    // Locks the `impl_basic_accessors!()` / `impl_dependencies_accessors!()`
    // field-name contract at the test surface for the `Workspace` trait too --
    // see the `MockPackage` impl above.
    changepacks_core::impl_basic_accessors!();
    changepacks_core::impl_dependencies_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
        Ok(())
    }
    fn language(&self) -> Language {
        self.language
    }
    impl_test_publish_commands!();
}
