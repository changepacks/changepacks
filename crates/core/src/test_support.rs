//! Shared mocks for this crate's unit tests and for sibling crates' test suites.
//!
//! This module plays a dual role, mirroring `changepacks-utils`'s own
//! `test_support`: it is compiled for this crate's unit tests via `#[cfg(test)]`,
//! and it is exported to sibling crates' test suites (e.g. `changepacks-cli`'s
//! command / option tests) via the `test-support` feature — declared
//! `#[cfg(any(test, feature = "test-support"))] pub mod test_support;` in
//! `lib.rs`. The feature path deliberately pulls in **no** dev-dependencies:
//! everything here only touches `std`, `anyhow`, `async-trait`, and this crate's
//! own production types / macros, so it compiles in a plain (non-dev) build
//! enabled solely by the feature. In a build with neither `test` nor the
//! feature the module is not compiled at all, so nothing ships in the default
//! public API.
//!
//! This consolidates the previously duplicated `MockPackage` / `MockWorkspace`
//! definitions from `package.rs`, `workspace.rs`, `project.rs`, and
//! `project_finder.rs` — and, via the `test-support` feature, the former
//! `crates/cli/src/test_support.rs` duplicate — behind one parameterized surface.
//!
//! Both mocks consume the production `crate::impl_basic_accessors!()` macro so
//! the macro's field-name contract is still locked by the type system: rename a
//! struct field (e.g. `is_changed` -> `changed`) and these mocks fail to
//! compile immediately, catching the regression before it ships downstream to
//! the language crates.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

/// Declarative macro to generate a mock struct and its inherent impl.
///
/// Expands to:
/// - A `#[derive(Debug)]` struct with the standard 7 fields
/// - An inherent impl with four constructors: `new`, `with_paths`, `same_path`, `with_language`
///
/// The only parameterization is the type name and the default manifest path literal
/// (e.g., `/test/Cargo.toml` for MockPackage, `/test/package.json` for MockWorkspace).
macro_rules! define_mock {
    ($type_name:ident, $default_path:expr, $default_relative:expr) => {
        #[derive(Debug)]
        pub struct $type_name {
            pub name: Option<String>,
            pub version: Option<String>,
            pub path: PathBuf,
            pub relative_path: PathBuf,
            pub language: Language,
            pub dependencies: HashSet<String>,
            pub is_changed: bool,
        }

        impl $type_name {
            /// Construct with an explicit `name` / `version` / `language`, defaulting
            /// the paths to the type's default manifest path.
            pub fn new(name: Option<&str>, version: Option<&str>, language: Language) -> Self {
                Self {
                    name: name.map(String::from),
                    version: version.map(String::from),
                    path: PathBuf::from($default_path),
                    relative_path: PathBuf::from($default_relative),
                    language,
                    dependencies: HashSet::new(),
                    is_changed: false,
                }
            }

            /// Construct with every field specified explicitly: `name` / `version` /
            /// `path` / `relative_path` / `language` (with `dependencies` empty and
            /// `is_changed` false).
            ///
            /// This is the fully-explicit form consumed by `changepacks-cli`'s test
            /// suites via the `test-support` feature, where each mock pins an exact
            /// path, relative path, and version — replacing the former CLI-local
            /// `MockPackage::new(name, version, path, relative_path, language)`.
            pub fn with_all(
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

            /// Construct with an explicit `path` / `relative_path`, defaulting the
            /// version to `1.0.0` and the language to `Node`.
            pub fn with_paths(name: Option<&str>, path: &str, relative_path: &str) -> Self {
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
            pub fn same_path(name: &str, path: &str) -> Self {
                Self::with_paths(Some(name), path, path)
            }

            /// Builder override for the mock's language.
            pub fn with_language(mut self, language: Language) -> Self {
                self.language = language;
                self
            }
        }
    };
}

define_mock!(MockPackage, "/test/Cargo.toml", "Cargo.toml");

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
    crate::impl_dependencies_accessors!();
    impl_test_publish_commands!();
}

define_mock!(MockWorkspace, "/test/package.json", "package.json");

#[async_trait]
impl Workspace for MockWorkspace {
    // Locks the `impl_basic_accessors!()` field-name contract at the test
    // surface for the `Workspace` trait too -- see the `MockPackage` impl above.
    crate::impl_basic_accessors!();
    crate::impl_dependencies_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }
    fn language(&self) -> Language {
        self.language
    }
    impl_test_publish_commands!();
}

#[derive(Debug)]
pub struct UnsupportedDryRunProject {
    pub path: PathBuf,
    pub dependencies: HashSet<String>,
}

#[async_trait]
impl Package for UnsupportedDryRunProject {
    fn name(&self) -> Option<&str> {
        Some("unsupported-dry-run")
    }

    fn version(&self) -> Option<&str> {
        Some("1.0.0")
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn relative_path(&self) -> &Path {
        Path::new("project.csproj")
    }

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }

    fn is_changed(&self) -> bool {
        false
    }

    fn language(&self) -> Language {
        Language::CSharp
    }

    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }

    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
    }

    fn set_changed(&mut self, _changed: bool) {}

    fn set_name(&mut self, _name: String) {}

    fn default_publish_command(&self) -> String {
        "echo publish".to_string()
    }

    fn default_dry_run_publish_command(&self) -> Option<String> {
        None
    }
}

#[async_trait]
impl Workspace for UnsupportedDryRunProject {
    fn name(&self) -> Option<&str> {
        Some("unsupported-dry-run")
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn relative_path(&self) -> &Path {
        Path::new("project.csproj")
    }

    fn version(&self) -> Option<&str> {
        Some("1.0.0")
    }

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }

    fn language(&self) -> Language {
        Language::CSharp
    }

    fn dependencies(&self) -> &HashSet<String> {
        &self.dependencies
    }

    fn add_dependency(&mut self, dependency: &str) {
        self.dependencies.insert(dependency.to_string());
    }

    fn is_changed(&self) -> bool {
        false
    }

    fn set_changed(&mut self, _changed: bool) {}

    fn set_name(&mut self, _name: String) {}

    fn default_publish_command(&self) -> String {
        "echo publish".to_string()
    }

    fn default_dry_run_publish_command(&self) -> Option<String> {
        None
    }
}
