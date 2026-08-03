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

/// Shared method bodies for [`UnsupportedDryRunProject`]'s two trait impls.
///
/// `Package` and `Workspace` declare the same accessor surface here, so the
/// hand-written `impl Package` / `impl Workspace` blocks were byte-identical
/// method for method. Emitting both from one macro keeps the fixture's
/// contract — name `unsupported-dry-run`, version `1.0.0`, relative path
/// `project.csproj`, language `CSharp`, never changed, and a
/// `default_dry_run_publish_command` of `None` — at a single surface, so the
/// two impls cannot drift apart.
///
/// `update_version` is deliberately NOT emitted here: these impls are
/// annotated with `#[async_trait]`, which rewrites `async fn` bodies while
/// parsing the impl block's tokens. A `macro_rules!` invocation is still an
/// opaque `ImplItem::Macro` at that point, so an `async fn` produced by this
/// macro would escape the rewrite and fail to match the desugared trait
/// signature.
macro_rules! impl_unsupported_dry_run_accessors {
    () => {
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

        fn is_changed(&self) -> bool {
            false
        }

        fn language(&self) -> Language {
            Language::CSharp
        }

        fn set_changed(&mut self, _changed: bool) {}

        fn set_name(&mut self, _name: String) {}

        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }

        fn default_dry_run_publish_command(&self) -> Option<String> {
            None
        }

        crate::impl_dependencies_accessors!();
    };
}

/// Shared method bodies for [`MockPackage`]'s and [`MockWorkspace`]'s trait impls.
///
/// `Package` and `Workspace` declare the same accessor surface for these two
/// mocks, so the hand-written `impl Package for MockPackage` and
/// `impl Workspace for MockWorkspace` blocks listed the same four items,
/// differing only in the order they were written. Emitting them from one macro
/// keeps the mocks' contract at a single surface so the two impls cannot drift
/// apart.
///
/// `crate::impl_basic_accessors!()` is still consumed here (rather than being
/// hand-written), so the macro's field-name contract stays locked by the type
/// system: rename a struct field (e.g. `is_changed` -> `changed`) and both
/// mocks fail to compile immediately.
///
/// `update_version` is deliberately NOT emitted here, for the same reason as
/// [`impl_unsupported_dry_run_accessors`]: `#[async_trait]` rewrites `async fn`
/// bodies while parsing the impl block's tokens, and a `macro_rules!`
/// invocation is still an opaque `ImplItem::Macro` at that point, so an
/// `async fn` produced by this macro would escape the rewrite and fail to match
/// the desugared trait signature.
macro_rules! impl_mock_project_accessors {
    () => {
        crate::impl_basic_accessors!();

        fn language(&self) -> Language {
            self.language
        }

        crate::impl_dependencies_accessors!();
        impl_test_publish_commands!();
    };
}

/// Declarative macro to generate a mock struct and its inherent impl.
///
/// Expands to:
/// - A `#[derive(Debug)]` struct with the standard 7 fields
/// - An inherent impl with four constructors: `new`, `with_paths`, `same_path`, `with_language`
///
/// The only parameterization is the type name and the default manifest path literal
/// (e.g., `/test/Cargo.toml` for `MockPackage`, `/test/package.json` for `MockWorkspace`).
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
            #[must_use]
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
    // `Package` impl uses (via `impl_mock_project_accessors!`). This mock exists
    // to prove the macro's field-name contract survives future edits: rename a
    // struct field and this fails to compile immediately. The struct fields
    // above are pinned to the macro's expected spellings
    // (`name: Option<String>`, `version: Option<String>`, `path: PathBuf`,
    // `relative_path: PathBuf`, `is_changed: bool`).
    impl_mock_project_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }
}

define_mock!(MockWorkspace, "/test/package.json", "package.json");

#[async_trait]
impl Workspace for MockWorkspace {
    // Locks the `impl_basic_accessors!()` field-name contract at the test
    // surface for the `Workspace` trait too -- see the `MockPackage` impl above.
    impl_mock_project_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct UnsupportedDryRunProject {
    pub path: PathBuf,
    pub dependencies: HashSet<String>,
}

/// Emits the eighteen regression tests that `Package`'s and `Workspace`'s
/// `mod tests` pin on the trait defaults produced by
/// `impl_shared_project_defaults!`, `impl_publish_flows!` and
/// `impl_publish_command_resolvers!`, plus the [`UnsupportedDryRunProject`]
/// fixture contract those defaults are exercised against.
///
/// Those three macros already de-duplicate the trait *bodies*, but the tests
/// pinning them were kept as two near-verbatim hand-maintained copies in
/// `package.rs` and `workspace.rs`, differing only in the mock type, the
/// missing-directory message, the echoed override marker and the trait used to
/// disambiguate [`UnsupportedDryRunProject`]'s two `dry_run_publish` methods.
/// Generating both copies from one surface means a default can no longer be
/// covered for one trait and silently left uncovered for the other.
///
/// Every test is emitted into the invoking `mod tests`, so the generated test
/// paths (`package::tests::*` / `workspace::tests::*`) and names are exactly
/// the ones the hand-written copies produced.
///
/// Parameters:
/// - `mock`: the mock project type (`MockPackage` / `MockWorkspace`), resolved
///   at the invocation site.
/// - `trait_name`: `Package` / `Workspace`, used to disambiguate the
///   `dry_run_publish` call on [`UnsupportedDryRunProject`], which implements
///   both traits.
/// - `kind`: `"package"` / `"workspace"`, the marker woven into the echoed
///   publish-override commands and the missing-directory fixture name.
/// - `dir_not_found`: the literal missing-directory message the publish flow is
///   expected to surface (`crate::publish::PACKAGE_DIR_NOT_FOUND` /
///   `WORKSPACE_DIR_NOT_FOUND` spelled out, so the assertion stays independent
///   of the constant under test).
/// - `publishable_test`: the name of the publishable-by-default test, which the
///   two hand-written copies spelled differently.
///
/// Tests that exist for only one of the two traits stay hand-written next to
/// this invocation: `test_inherits_workspace_version_default` and
/// `test_workspace_root_path_default` in `package.rs`; the three
/// `test_get_dry_run_publish_command_*` tests and
/// `test_update_workspace_dependencies_default` in `workspace.rs`.
#[cfg(test)]
macro_rules! shared_project_default_tests {
    (
        mock: $mock:ident,
        trait_name: $trait_name:ident,
        kind: $kind:literal,
        dir_not_found: $dir_not_found:literal,
        publishable_test: $publishable_test:ident $(,)?
    ) => {
        #[test]
        fn test_check_changed_already_changed() {
            let mut project =
                $mock::with_paths(Some("test"), "/project/package.json", "package.json");
            project.is_changed = true;

            // Should return early if already changed
            project
                .check_changed(std::path::Path::new("/project/src/index.js"))
                .unwrap();
            assert!(project.is_changed());
        }

        #[rstest::rstest]
        // A file inside the project dir marks it changed; a changepack log or a
        // file that belongs to another project does not.
        #[case("/project/src/index.js", true)]
        #[case("/project/.changepacks/change.json", false)]
        #[case("/other-project/src/index.js", false)]
        fn test_check_changed(#[case] changed_path: &str, #[case] expected: bool) {
            let mut project =
                $mock::with_paths(Some("test"), "/project/package.json", "package.json");
            project
                .check_changed(std::path::Path::new(changed_path))
                .unwrap();
            assert_eq!(project.is_changed(), expected);
        }

        #[test]
        fn $publishable_test() {
            let project = $mock::with_paths(Some("test"), "/project/package.json", "package.json");

            assert!(project.is_publishable_by_default());
            assert_eq!(
                project.is_dry_run_publishable_by_default(),
                project.is_publishable_by_default()
            );
        }

        #[test]
        fn test_get_publish_command_by_path() {
            let project = $mock::with_paths(
                Some("test"),
                "/project/package.json",
                "packages/core/package.json",
            );
            let mut publish = std::collections::BTreeMap::new();
            publish.insert(
                "packages/core/package.json".to_string(),
                "custom publish".to_string(),
            );
            let config = crate::Config {
                publish,
                ..Default::default()
            };

            assert_eq!(project.get_publish_command(&config), "custom publish");
        }

        #[rstest::rstest]
        #[case(crate::Language::Node, "node", "npm publish --access public")]
        #[case(crate::Language::Python, "python", "poetry publish")]
        #[case(crate::Language::Rust, "rust", "cargo publish")]
        #[case(crate::Language::Dart, "dart", "dart pub publish")]
        #[case(crate::Language::Java, "java", "./gradlew publish")]
        #[case(crate::Language::CSharp, "csharp", "dotnet nuget push")]
        fn test_get_publish_command_by_language(
            #[case] language: crate::Language,
            #[case] key: &str,
            #[case] command: &str,
        ) {
            let project = $mock::with_paths(Some("test"), "/project/manifest", "manifest")
                .with_language(language);
            let mut publish = std::collections::BTreeMap::new();
            publish.insert(key.to_string(), command.to_string());
            let config = crate::Config {
                publish,
                ..Default::default()
            };

            assert_eq!(project.get_publish_command(&config), command);
        }

        #[test]
        fn test_get_publish_command_default() {
            let project = $mock::with_paths(Some("test"), "/project/package.json", "package.json");
            let config = crate::Config::default();

            assert_eq!(project.get_publish_command(&config), "echo publish");
        }

        #[tokio::test]
        async fn test_publish_success() {
            let temp_dir = std::env::temp_dir();
            let path = temp_dir.join("package.json");
            let project = $mock::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
            let config = crate::Config::default();

            // This will run "echo publish" which should succeed
            let output = project.publish(&config).await.unwrap();
            assert!(output.success);
            assert!(output.stdout.contains("publish"));
        }

        #[tokio::test]
        async fn test_publish_uses_project_path_override() {
            let path = std::env::temp_dir().join("package.json");
            let project = $mock::with_paths(
                Some("test"),
                path.to_str().unwrap(),
                "packages/core/package.json",
            );
            let config = crate::Config {
                publish: std::collections::BTreeMap::from([(
                    "packages/core/package.json".to_string(),
                    concat!("echo ", $kind, "-path-override").to_string(),
                )]),
                ..Default::default()
            };

            let output = project.publish(&config).await.unwrap();

            assert!(output.success);
            assert!(output.stdout.contains(concat!($kind, "-path-override")));
        }

        #[tokio::test]
        async fn test_publish_uses_language_override() {
            let path = std::env::temp_dir().join("package.json");
            let project = $mock::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
            let config = crate::Config {
                publish: std::collections::BTreeMap::from([(
                    "node".to_string(),
                    concat!("echo ", $kind, "-language-override").to_string(),
                )]),
                ..Default::default()
            };

            let output = project.publish(&config).await.unwrap();

            assert!(output.success);
            assert!(output.stdout.contains(concat!($kind, "-language-override")));
        }

        #[tokio::test]
        async fn test_publish_failure() {
            let temp_dir = std::env::temp_dir();
            let path = temp_dir.join("package.json");
            let project = $mock::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
            let mut publish = std::collections::BTreeMap::new();
            let fail_cmd = if cfg!(target_os = "windows") {
                "cmd /c exit 1"
            } else {
                "exit 1"
            };
            publish.insert("node".to_string(), fail_cmd.to_string());
            let config = crate::Config {
                publish,
                ..Default::default()
            };

            let output = project.publish(&config).await.unwrap();
            assert!(!output.success);
        }

        #[tokio::test]
        async fn test_publish_no_parent_directory() {
            let project = $mock::with_paths(Some("test"), "", "");
            let config = crate::Config::default();
            let result = project.publish(&config).await;
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains($dir_not_found));
        }

        #[tokio::test]
        async fn test_publish_reports_missing_current_directory() {
            let missing_dir = std::env::temp_dir().join(format!(
                "changepacks_missing_{}_dir_{}",
                $kind,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&missing_dir);
            let path = missing_dir.join("package.json");
            let project = $mock::with_paths(Some("test"), path.to_str().unwrap(), "package.json");

            let result = project.publish(&crate::Config::default()).await;

            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_dry_run_publish_uses_project_path_override() {
            let path = std::env::temp_dir().join("package.json");
            let project = $mock::with_paths(
                Some("test"),
                path.to_str().unwrap(),
                "packages/core/package.json",
            );
            let config = crate::Config {
                publish_dry_run: std::collections::BTreeMap::from([(
                    "packages/core/package.json".to_string(),
                    concat!("echo ", $kind, "-dry-path-override").to_string(),
                )]),
                ..Default::default()
            };

            let output = project.dry_run_publish(&config).await.unwrap().unwrap();

            assert!(output.success);
            assert!(output.stdout.contains(concat!($kind, "-dry-path-override")));
        }

        #[tokio::test]
        async fn test_dry_run_publish_uses_language_override() {
            let path = std::env::temp_dir().join("package.json");
            let project = $mock::with_paths(Some("test"), path.to_str().unwrap(), "package.json");
            let config = crate::Config {
                publish_dry_run: std::collections::BTreeMap::from([(
                    "node".to_string(),
                    concat!("echo ", $kind, "-dry-language-override").to_string(),
                )]),
                ..Default::default()
            };

            let output = project.dry_run_publish(&config).await.unwrap().unwrap();

            assert!(output.success);
            assert!(
                output
                    .stdout
                    .contains(concat!($kind, "-dry-language-override"))
            );
        }

        #[tokio::test]
        async fn test_dry_run_publish_uses_default_command() {
            let path = std::env::temp_dir().join("package.json");
            let project = $mock::with_paths(Some("test"), path.to_str().unwrap(), "package.json");

            let output = project
                .dry_run_publish(&crate::Config::default())
                .await
                .unwrap()
                .unwrap();

            assert!(output.success);
            assert!(output.stdout.contains("publish --dry-run"));
        }

        #[tokio::test]
        async fn test_dry_run_publish_returns_none_when_unsupported() {
            let project = crate::test_support::UnsupportedDryRunProject {
                path: std::env::temp_dir().join("project.csproj"),
                dependencies: std::collections::HashSet::new(),
            };

            let output = $trait_name::dry_run_publish(&project, &crate::Config::default())
                .await
                .unwrap();

            assert!(output.is_none());
        }

        #[tokio::test]
        async fn test_unsupported_dry_run_project_fixture_contract() {
            // Pins every constant `impl_unsupported_dry_run_accessors!` emits,
            // so the fixture the dry-run-unsupported test above depends on
            // cannot silently drift. Emitted once per trait because
            // `UnsupportedDryRunProject` implements `Package` and `Workspace`
            // from two separate impl blocks, each with its own
            // `update_version` body.
            let path = std::env::temp_dir().join("project.csproj");
            let mut project = crate::test_support::UnsupportedDryRunProject {
                path: path.clone(),
                dependencies: std::collections::HashSet::new(),
            };

            assert_eq!($trait_name::name(&project), Some("unsupported-dry-run"));
            assert_eq!($trait_name::version(&project), Some("1.0.0"));
            assert_eq!($trait_name::path(&project), path.as_path());
            assert_eq!(
                $trait_name::relative_path(&project),
                std::path::Path::new("project.csproj")
            );
            assert_eq!($trait_name::language(&project), crate::Language::CSharp);
            assert!(!$trait_name::is_changed(&project));
            assert_eq!(
                $trait_name::default_publish_command(&project),
                "echo publish"
            );
            assert_eq!($trait_name::default_dry_run_publish_command(&project), None);

            // Both setters are deliberate no-ops on this fixture: the two
            // constants they would normally move must survive the calls.
            $trait_name::set_changed(&mut project, true);
            $trait_name::set_name(&mut project, "renamed".to_string());
            assert!(
                !$trait_name::is_changed(&project),
                "set_changed must stay a no-op on the fixture"
            );
            assert_eq!(
                $trait_name::name(&project),
                Some("unsupported-dry-run"),
                "set_name must stay a no-op on the fixture"
            );

            // The fixture's `update_version` is a success stub, so it reports
            // `Ok(())` and leaves the pinned version untouched.
            $trait_name::update_version(&mut project, crate::UpdateType::Patch)
                .await
                .unwrap();
            assert_eq!($trait_name::version(&project), Some("1.0.0"));
        }

        #[test]
        fn test_set_name_updates_via_impl_basic_accessors_macro() {
            // Regression guard for the shared-macro accessor contract:
            // the mock's trait impl uses the shared
            // `crate::impl_basic_accessors!()` macro, so `set_name` MUST update
            // the underlying `name` field (not fall through to the trait's
            // default no-op). If the macro's field-name contract silently
            // regresses (say, someone renames `name` on the mock and the macro
            // loses sight of it), the mock will fail to compile; this test then
            // locks the runtime behavior after compilation.
            let mut project =
                $mock::with_paths(Some("original"), "/project/package.json", "package.json");
            project.set_name("new-name".to_string());
            assert_eq!(project.name(), Some("new-name"));
        }
    };
}

#[cfg(test)]
pub(crate) use shared_project_default_tests;

#[async_trait]
impl Package for UnsupportedDryRunProject {
    impl_unsupported_dry_run_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Workspace for UnsupportedDryRunProject {
    impl_unsupported_dry_run_accessors!();

    async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
        Ok(())
    }
}
