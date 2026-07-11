//! Shared `#[cfg(test)]` mocks for this crate's unit tests.
//!
//! Declared `#[cfg(test)] pub(crate) mod test_support;` in `lib.rs`, so nothing
//! here is compiled into a non-test build or shipped in the public API. This
//! consolidates the previously duplicated `MockPackage` / `MockWorkspace`
//! definitions from `package.rs`, `workspace.rs`, `project.rs`, and
//! `project_finder.rs` behind one parameterized surface.
//!
//! Both mocks consume the production `crate::impl_basic_accessors!()` macro so
//! the macro's field-name contract is still locked by the type system: rename a
//! struct field (e.g. `is_changed` -> `changed`) and these mocks fail to
//! compile immediately, catching the regression before it ships downstream to
//! the language crates.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;

use crate::{Language, Package, UpdateType, Workspace};

/// Shared test-only publish command defaults for both mocks.
///
/// Kept as a macro so the `"echo publish"` / `"echo publish --dry-run"`
/// contract lives at ONE surface across the `Package` and `Workspace` impls,
/// mirroring how the real impls consume `impl_const_publish_commands!`.
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
    /// Construct with an explicit `name` / `version` / `language`, defaulting
    /// the paths to `/test/Cargo.toml` (relative `Cargo.toml`).
    pub(crate) fn new(name: Option<&str>, version: Option<&str>, language: Language) -> Self {
        Self {
            name: name.map(String::from),
            version: version.map(String::from),
            path: PathBuf::from("/test/Cargo.toml"),
            relative_path: PathBuf::from("Cargo.toml"),
            language,
            dependencies: HashSet::new(),
            is_changed: false,
        }
    }

    /// Construct with an explicit `path` / `relative_path`, defaulting the
    /// version to `1.0.0` and the language to `Node`.
    pub(crate) fn with_paths(name: Option<&str>, path: &str, relative_path: &str) -> Self {
        Self {
            name: name.map(String::from),
            version: Some("1.0.0".to_string()),
            path: PathBuf::from(path),
            relative_path: PathBuf::from(relative_path),
            language: Language::Node,
            dependencies: HashSet::new(),
            is_changed: false,
        }
    }

    /// Construct with a required `name` and a single `path` reused as both
    /// `path` and `relative_path` (version `1.0.0`, language `Node`).
    pub(crate) fn same_path(name: &str, path: &str) -> Self {
        Self::with_paths(Some(name), path, path)
    }

    /// Builder override for the mock's language.
    pub(crate) fn with_language(mut self, language: Language) -> Self {
        self.language = language;
        self
    }
}

#[async_trait]
impl Package for MockPackage {
    // Consumes the same `impl_basic_accessors!()` macro that every real-world
    // `Package` impl uses. This mock exists to prove the macro's field-name
    // contract survives future edits: rename a struct field and this fails to
    // compile immediately. The struct fields above are pinned to the macro's
    // expected spellings (`name: Option<String>`, `version: Option<String>`,
    // `path: PathBuf`, `relative_path: PathBuf`, `is_changed: bool`).
    crate::impl_basic_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }
    fn language(&self) -> Language {
        self.language
    }
    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }
    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
    }
    impl_test_publish_commands!();
}

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
    /// Construct with an explicit `name` / `version` / `language`, defaulting
    /// the paths to `/test/package.json` (relative `package.json`).
    pub(crate) fn new(name: Option<&str>, version: Option<&str>, language: Language) -> Self {
        Self {
            name: name.map(String::from),
            version: version.map(String::from),
            path: PathBuf::from("/test/package.json"),
            relative_path: PathBuf::from("package.json"),
            language,
            dependencies: HashSet::new(),
            is_changed: false,
        }
    }

    /// Construct with an explicit `path` / `relative_path`, defaulting the
    /// version to `1.0.0` and the language to `Node`.
    pub(crate) fn with_paths(name: Option<&str>, path: &str, relative_path: &str) -> Self {
        Self {
            name: name.map(String::from),
            version: Some("1.0.0".to_string()),
            path: PathBuf::from(path),
            relative_path: PathBuf::from(relative_path),
            language: Language::Node,
            dependencies: HashSet::new(),
            is_changed: false,
        }
    }

    /// Construct with a required `name` and a single `path` reused as both
    /// `path` and `relative_path` (version `1.0.0`, language `Node`).
    pub(crate) fn same_path(name: &str, path: &str) -> Self {
        Self::with_paths(Some(name), path, path)
    }

    /// Builder override for the mock's language.
    pub(crate) fn with_language(mut self, language: Language) -> Self {
        self.language = language;
        self
    }
}

#[async_trait]
impl Workspace for MockWorkspace {
    // Locks the `impl_basic_accessors!()` field-name contract at the test
    // surface for the `Workspace` trait too -- see the `MockPackage` impl above.
    crate::impl_basic_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }
    fn language(&self) -> Language {
        self.language
    }
    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }
    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
    }
    impl_test_publish_commands!();
}
