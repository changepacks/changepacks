use std::path::Path;

use crate::project::Project;
use anyhow::Result;
use async_trait::async_trait;

/// Expand to the identical `projects()` / `projects_mut()` accessor pair
/// used by every finder whose backing store is a `HashMap<PathBuf,
/// Project>` — currently every language finder (Node, Python, Dart,
/// Rust, CSharp, Java) shares byte-identical bodies through this macro.
///
/// Invoked from inside an `impl ProjectFinder for XxxProjectFinder`
/// block; expands to two methods that collect the map's values into a
/// `Vec` of borrows. Byte-identical expansion — the previously
/// hand-rolled bodies:
///
/// ```ignore
/// fn projects(&self)     -> Vec<&Project>     { self.projects.values().collect::<Vec<_>>() }
/// fn projects_mut(&mut self) -> Vec<&mut Project> { self.projects.values_mut().collect::<Vec<_>>() }
/// ```
///
/// are replaced 1:1 by a single `impl_projects_hashmap_accessors!()`
/// invocation. `$crate::Project` is used so callers do not have to have
/// `Project` in scope at the invocation site — though every current
/// caller already does via `use changepacks_core::{Project,
/// ProjectFinder};`, so the macro is fully backward-compatible with the
/// existing import shape.
///
/// Consumer requirement: the struct must have a
/// `projects: HashMap<PathBuf, Project>` field with those exact
/// spellings. Sibling fields (e.g. `is_workspace_cache` on
/// `CSharpProjectFinder`, `workspace_package_version` on
/// `RustProjectFinder`) are untouched by the macro — it only reads
/// `self.projects`.
#[macro_export]
macro_rules! impl_projects_hashmap_accessors {
    () => {
        fn projects(&self) -> ::std::vec::Vec<&$crate::Project> {
            self.projects.values().collect::<::std::vec::Vec<_>>()
        }
        fn projects_mut(&mut self) -> ::std::vec::Vec<&mut $crate::Project> {
            self.projects.values_mut().collect::<::std::vec::Vec<_>>()
        }
    };
}

/// Expand to the identical `dependencies()` / `add_dependency()` method
/// pair used by every `Package` / `Workspace` impl whose backing store
/// is a `dependencies: HashSet<String>` field — currently every
/// language crate's `package.rs` and `workspace.rs` (Node, Python,
/// Rust, Dart, CSharp, Java) shares byte-identical bodies through this
/// macro.
///
/// Invoked from inside an `impl Package for XxxPackage` or `impl
/// Workspace for XxxWorkspace` block; expands to two methods that
/// borrow the dependency set immutably and insert into it mutably.
/// Byte-identical expansion — the previously hand-rolled bodies:
///
/// ```ignore
/// fn dependencies(&self)                       -> &HashSet<String> { &self.dependencies }
/// fn add_dependency(&mut self, dependency: &str)                    { self.dependencies.insert(dependency.to_string()); }
/// ```
///
/// are replaced 1:1 by a single `impl_dependencies_accessors!()`
/// invocation. Fully-qualified `::std::collections::HashSet` and
/// `::std::string::String` make the macro hygienic — callers do not
/// need those types in scope at the invocation site (though every
/// current caller already uses `use std::collections::HashSet;`, so
/// the macro is fully backward-compatible with the existing import
/// shape).
///
/// Consumer requirement: the struct must have a `dependencies:
/// HashSet<String>` field with those exact spellings. Sibling fields
/// (e.g. `is_changed` on every impl, `workspace_version_inherited` on
/// `RustPackage`) are untouched by the macro — it only reads and
/// mutates `self.dependencies`.
#[macro_export]
macro_rules! impl_dependencies_accessors {
    () => {
        fn dependencies(&self) -> &::std::collections::HashSet<::std::string::String> {
            &self.dependencies
        }
        fn add_dependency(&mut self, dependency: &str) {
            self.dependencies.insert(dependency.to_string());
        }
    };
}

/// Expand to the identical `default_publish_command` /
/// `default_dry_run_publish_command` method pair used by every language
/// crate whose defaults are const-based (Python, Dart, Java, CSharp) —
/// i.e. `default_publish_command` returns `SOME_CONST.to_string()` and
/// `default_dry_run_publish_command` either returns
/// `Some(SOME_OTHER_CONST.to_string())` or `None`.
///
/// Invoked from inside an `impl Package for XxxPackage` or `impl
/// Workspace for XxxWorkspace` block. Two forms:
///
/// - Two-arg form (Python / Dart / Java): both `publish` and `dry-run`
///   are const strings, `default_dry_run_publish_command` returns
///   `Some($dry_run.to_string())`.
/// - One-arg form (CSharp): only the real publish is a const string,
///   `default_dry_run_publish_command` returns `None` (the actual C#
///   dry-run flow is managed via the `dry_run_publish` trait override
///   in `crates/csharp/src/dry_run.rs`).
///
/// Byte-identical expansion — the previously hand-rolled bodies:
///
/// ```ignore
/// fn default_publish_command(&self) -> String { crate::PUBLISH_COMMAND.to_string() }
/// fn default_dry_run_publish_command(&self) -> Option<String> {
///     Some(crate::DRY_RUN_PUBLISH_COMMAND.to_string())
/// }
/// ```
///
/// are replaced 1:1 by a single
/// `impl_const_publish_commands!(crate::PUBLISH_COMMAND, crate::DRY_RUN_PUBLISH_COMMAND);`
/// invocation. Fully-qualified `::std::string::String` and
/// `::std::option::Option` make the macro hygienic — callers do not
/// need those types in scope at the invocation site.
///
/// Node cannot use this macro because its publish command is
/// determined at runtime via `detect_package_manager_recursive`, not
/// from a compile-time const; see `impl_node_publish_wiring!()` in
/// `crates/node/src/lib.rs`. Both `RustPackage` and `RustWorkspace`
/// use the two-arg form (with `PUBLISH_COMMAND` /
/// `DRY_RUN_PUBLISH_COMMAND` and `WORKSPACE_PUBLISH_COMMAND` /
/// `WORKSPACE_DRY_RUN_PUBLISH_COMMAND` respectively) — the macro's
/// `$publish:path` parameter accepts either const path.
#[macro_export]
macro_rules! impl_const_publish_commands {
    ($publish:path, $dry_run:path) => {
        fn default_publish_command(&self) -> ::std::string::String {
            $publish.to_string()
        }
        fn default_dry_run_publish_command(&self) -> ::std::option::Option<::std::string::String> {
            ::std::option::Option::Some($dry_run.to_string())
        }
    };
    // CSharp variant: `dotnet nuget push` has no built-in `--dry-run`
    // mode, so the default returns `None`. The actual dry-run flow
    // lives in `CSharpPackage::dry_run_publish` / `CSharpWorkspace::
    // dry_run_publish` (see `crates/csharp/src/dry_run.rs::
    // resolve_and_run_dry_run`), which honors `config.publishDryRun`
    // overrides first and falls back to a managed `dotnet pack` +
    // `dotnet nuget push` against ephemeral `tempfile::TempDir`
    // directories when no override is set.
    ($publish:path) => {
        fn default_publish_command(&self) -> ::std::string::String {
            $publish.to_string()
        }
        fn default_dry_run_publish_command(&self) -> ::std::option::Option<::std::string::String> {
            ::std::option::Option::None
        }
    };
}

/// Expand to the seven identical basic accessor bodies (`name`,
/// `version`, `path`, `relative_path`, `is_changed`, `set_changed`,
/// `set_name`) used by every `Package` / `Workspace` impl in every
/// language crate (Node, Python, Rust, Dart, CSharp, Java — 12 impls,
/// 84 method bodies before this macro).
///
/// Invoked from inside an `impl Package for XxxPackage` or `impl
/// Workspace for XxxWorkspace` block; expands to the seven byte-
/// identical bodies:
///
/// ```ignore
/// fn name(&self) -> Option<&str>          { self.name.as_deref() }
/// fn version(&self) -> Option<&str>       { self.version.as_deref() }
/// fn path(&self) -> &Path                 { &self.path }
/// fn relative_path(&self) -> &Path        { &self.relative_path }
/// fn is_changed(&self) -> bool            { self.is_changed }
/// fn set_changed(&mut self, changed: bool){ self.is_changed = changed; }
/// fn set_name(&mut self, name: String)    { self.name = Some(name); }
/// ```
///
/// Fully-qualified `::std::primitive::str`, `::std::path::Path`,
/// `::std::primitive::bool`, `::std::string::String`, and
/// `::std::option::Option` make the macro hygienic — callers do not
/// need those types in scope at the invocation site (though every
/// current caller already has `Path` and `String` in scope, so the
/// macro is fully backward-compatible with the existing import shape).
///
/// Consumer requirement: the struct must have `name: Option<String>`,
/// `version: Option<String>`, `path: PathBuf`, `relative_path:
/// PathBuf`, and `is_changed: bool` fields with those exact spellings.
/// Language-specific overrides (e.g. `RustPackage::
/// inherits_workspace_version`, `RustPackage::workspace_root_path`) are
/// untouched by this macro — it only touches the seven trivial
/// accessors listed above. Sibling fields (e.g.
/// `workspace_version_inherited` on `RustPackage`, `is_workspace_cache`
/// on `CSharpProjectFinder`) are likewise untouched.
#[macro_export]
macro_rules! impl_basic_accessors {
    () => {
        fn name(&self) -> ::std::option::Option<&::std::primitive::str> {
            self.name.as_deref()
        }
        fn version(&self) -> ::std::option::Option<&::std::primitive::str> {
            self.version.as_deref()
        }
        fn path(&self) -> &::std::path::Path {
            &self.path
        }
        fn relative_path(&self) -> &::std::path::Path {
            &self.relative_path
        }
        fn is_changed(&self) -> ::std::primitive::bool {
            self.is_changed
        }
        fn set_changed(&mut self, changed: ::std::primitive::bool) {
            self.is_changed = changed;
        }
        fn set_name(&mut self, name: ::std::string::String) {
            self.name = ::std::option::Option::Some(name);
        }
    };
}

/// Expand to the identical `pub fn new(name, version, path, relative_path)`
/// constructor body used by every "plain 5-basic-field" language crate's
/// `Package` / `Workspace` inherent-impl — currently Node, Python, Dart,
/// CSharp, Java (10 impls, all with byte-identical 7-line struct-literal
/// bodies before this macro).
///
/// Invoked from inside an `impl XxxPackage { ... }` or `impl XxxWorkspace
/// { ... }` block; expands to the byte-identical constructor:
///
/// ```ignore
/// #[must_use]
/// pub fn new(
///     name: Option<String>,
///     version: Option<String>,
///     path: PathBuf,
///     relative_path: PathBuf,
/// ) -> Self {
///     Self {
///         name,
///         version,
///         path,
///         relative_path,
///         is_changed: false,
///         dependencies: HashSet::new(),
///     }
/// }
/// ```
///
/// Consumer requirement: the struct must have `name: Option<String>`,
/// `version: Option<String>`, `path: PathBuf`, `relative_path: PathBuf`,
/// `is_changed: bool`, and `dependencies: HashSet<String>` fields with
/// those exact spellings — the same "struct-field contract"
/// `impl_basic_accessors!()` already pins. `RustPackage` /
/// `RustWorkspace` intentionally stay hand-rolled because `RustPackage`
/// carries two extra fields (`workspace_version_inherited`,
/// `workspace_root`) and takes a distinct constructor signature; the
/// macro is fully backward-compatible with the existing import shape
/// because all types are fully-qualified.
///
/// Fully-qualified `::std::option::Option`, `::std::string::String`,
/// `::std::path::PathBuf`, `::std::collections::HashSet` make the macro
/// hygienic — callers do not need those types in scope at the invocation
/// site (though every current caller already has
/// `use std::path::PathBuf;` and `use std::collections::HashSet;`, so the
/// macro is fully backward-compatible with the existing import shape).
///
/// The `#[must_use]` attribute is baked into the expansion so downstream
/// lint parity with the previously hand-rolled `#[must_use] pub fn new`
/// bodies is preserved — a caller that discards the returned `Self` will
/// still trip clippy's `unused_must_use` at the call site.
#[macro_export]
macro_rules! impl_default_new {
    () => {
        #[must_use]
        pub fn new(
            name: ::std::option::Option<::std::string::String>,
            version: ::std::option::Option<::std::string::String>,
            path: ::std::path::PathBuf,
            relative_path: ::std::path::PathBuf,
        ) -> Self {
            Self {
                name,
                version,
                path,
                relative_path,
                is_changed: false,
                dependencies: ::std::collections::HashSet::new(),
            }
        }
    };
}

/// Expand to the identical `fn language(&self) -> Language { Language::X }`
/// single-line accessor used by every `Package` / `Workspace` impl in every
/// language crate (Node, Python, Rust, Dart, CSharp, Java — 12 impls, all
/// three-line bodies before this macro).
///
/// Invoked from inside an `impl Package for XxxPackage` or `impl Workspace
/// for XxxWorkspace` block with the specific `Language` variant for that
/// crate; expands to the one method body:
///
/// ```ignore
/// fn language(&self) -> Language { Language::Node }
/// ```
///
/// (etc. for Python, Rust, Dart, CSharp, Java). `$crate::Language` is used
/// so callers do not have to have `Language` in scope at the invocation
/// site — though every current caller already does via
/// `use changepacks_core::{Language, ...};`, so the macro is fully
/// backward-compatible with the existing import shape.
///
/// Consolidated alongside `impl_basic_accessors!()` /
/// `impl_dependencies_accessors!()` /
/// `impl_const_publish_commands!()` /
/// `impl_projects_hashmap_accessors!()` because this was the last obvious
/// byte-identical accessor that had escaped the macro-consolidation
/// sweeps in prior iterations.
#[macro_export]
macro_rules! impl_language {
    ($lang:expr) => {
        fn language(&self) -> $crate::Language {
            $lang
        }
    };
}

/// Returns `true` when `path` refers to an existing regular file.
///
/// AGENTS.md rule: never blocking I/O in async — use `tokio::fs::metadata`.
/// `unwrap_or(false)` mirrors the previous inline `is_file()` semantics on
/// stat errors (broken symlink, permission denied, missing path): treat as
/// "not a regular file" and short-circuit, rather than propagating an error
/// the caller would silently ignore.
///
/// Shared between `ProjectFinder::matches_project_file` (name-based match
/// used by every language) and `CSharpProjectFinder::visit` (extension-based
/// match) so the byte-identical stat + `is_file()` fallthrough lives in ONE
/// place. Public so cross-crate callers (e.g. `changepacks-csharp`) can
/// reuse it via the re-export from `changepacks_core::lib.rs`.
pub async fn is_regular_file(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Visitor pattern for discovering projects by walking the git tree.
///
/// Each language implements this trait to detect its project files (package.json, Cargo.toml, etc.)
/// and build a collection of projects. The `visit` method is called for each file in the git tree.
#[async_trait]
pub trait ProjectFinder: std::fmt::Debug + Send + Sync {
    fn projects(&self) -> Vec<&Project>;
    fn projects_mut(&mut self) -> Vec<&mut Project>;
    fn project_files(&self) -> &[&str];
    /// # Errors
    /// Returns error if the file visitation fails.
    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()>;
    /// Whether `path` is a project manifest file recognized by this finder.
    ///
    /// Returns `Ok(false)` for directories and files whose name is not in
    /// `project_files()`. Used by language-specific `visit()` implementations
    /// to gate manifest parsing on file-name matching. `CSharpProjectFinder`
    /// intentionally uses `.extension()` matching instead and does not call
    /// this method.
    ///
    /// Check order is name-first, stat-last: on a monorepo with N tracked
    /// files where only K match any recognized manifest name (typically
    /// K ≪ N), the previous stat-then-name shape issued 5 × N async
    /// `tokio::fs::metadata` syscalls across every non-CSharp language
    /// finder invocation. Reversing the order collapses that to ~K stats
    /// total. Missing/non-UTF-8 file names cannot possibly match ASCII
    /// manifest names anyway, so returning `Ok(false)` early is
    /// semantically identical to the previous with_context error paths
    /// — which were unreachable for git-index-derived paths.
    ///
    /// # Errors
    /// Currently never returns an error; the `Result` return is preserved
    /// so future implementations may return their own errors without a
    /// signature break.
    async fn matches_project_file(&self, path: &Path) -> Result<bool> {
        let Some(name_os) = path.file_name() else {
            return Ok(false);
        };
        let Some(name) = name_os.to_str() else {
            return Ok(false);
        };
        if !self.project_files().contains(&name) {
            return Ok(false);
        }
        Ok(is_regular_file(path).await)
    }
    /// # Errors
    /// Returns error if checking changed status fails for any project.
    fn check_changed(&mut self, path: &Path) -> Result<()> {
        for project in self.projects_mut() {
            project.check_changed(path)?;
        }
        Ok(())
    }
    /// Post-visit processing hook for resolving deferred state (e.g., workspace-inherited versions).
    /// Called once after all `visit()` calls complete.
    /// # Errors
    /// Returns error if finalization fails.
    #[cfg(not(tarpaulin_include))]
    async fn finalize(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Language, Package, UpdateType, Workspace};
    use async_trait::async_trait;
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[derive(Debug)]
    struct MockPackage {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        is_changed: bool,
        dependencies: HashSet<String>,
    }

    impl MockPackage {
        fn new(name: &str, path: &str) -> Self {
            Self {
                name: Some(name.to_string()),
                version: Some("1.0.0".to_string()),
                path: PathBuf::from(path),
                relative_path: PathBuf::from(path),
                is_changed: false,
                dependencies: HashSet::new(),
            }
        }
    }

    #[async_trait]
    impl Package for MockPackage {
        // Locks the `impl_basic_accessors!()` field-name contract at the
        // test surface — see the sibling mock in `package.rs::tests`.
        crate::impl_basic_accessors!();

        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            Ok(())
        }
        fn language(&self) -> Language {
            Language::Node
        }
        fn dependencies(&self) -> &HashSet<String> {
            &self.dependencies
        }
        fn add_dependency(&mut self, dep: &str) {
            self.dependencies.insert(dep.to_string());
        }
        fn default_publish_command(&self) -> String {
            "echo test".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo test --dry-run".to_string())
        }
        fn inherits_workspace_version(&self) -> bool {
            false
        }
        fn workspace_root_path(&self) -> Option<&Path> {
            None
        }
    }

    #[derive(Debug)]
    struct MockWorkspace {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        is_changed: bool,
        dependencies: HashSet<String>,
    }

    impl MockWorkspace {
        fn new(name: &str, path: &str) -> Self {
            Self {
                name: Some(name.to_string()),
                version: Some("1.0.0".to_string()),
                path: PathBuf::from(path),
                relative_path: PathBuf::from(path),
                is_changed: false,
                dependencies: HashSet::new(),
            }
        }
    }

    #[async_trait]
    impl Workspace for MockWorkspace {
        // Locks the `impl_basic_accessors!()` field-name contract at the
        // test surface — see the sibling mock in `package.rs::tests`.
        crate::impl_basic_accessors!();

        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            Ok(())
        }
        fn language(&self) -> Language {
            Language::Node
        }
        fn dependencies(&self) -> &HashSet<String> {
            &self.dependencies
        }
        fn add_dependency(&mut self, dep: &str) {
            self.dependencies.insert(dep.to_string());
        }
        fn default_publish_command(&self) -> String {
            "echo test".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo test --dry-run".to_string())
        }
    }

    #[derive(Debug)]
    struct MockProjectFinder {
        projects: Vec<Project>,
    }

    impl MockProjectFinder {
        fn new() -> Self {
            Self { projects: vec![] }
        }

        fn with_package(mut self, package: MockPackage) -> Self {
            self.projects.push(Project::Package(Box::new(package)));
            self
        }

        fn with_workspace(mut self, workspace: MockWorkspace) -> Self {
            self.projects.push(Project::Workspace(Box::new(workspace)));
            self
        }
    }

    #[async_trait]
    impl ProjectFinder for MockProjectFinder {
        fn projects(&self) -> Vec<&Project> {
            self.projects.iter().collect()
        }

        fn projects_mut(&mut self) -> Vec<&mut Project> {
            self.projects.iter_mut().collect()
        }

        fn project_files(&self) -> &[&str] {
            &["package.json"]
        }

        async fn visit(&mut self, _path: &Path, _relative_path: &Path) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_project_finder_check_changed() {
        let package = MockPackage::new("test", "/project/package.json");
        let mut finder = MockProjectFinder::new().with_package(package);

        // Check a file that's in the project directory
        finder
            .check_changed(Path::new("/project/src/index.js"))
            .unwrap();

        // The project should be marked as changed
        assert!(finder.projects()[0].is_changed());
    }

    #[test]
    fn test_project_finder_check_changed_multiple_projects() {
        let package1 = MockPackage::new("pkg1", "/project1/package.json");
        let package2 = MockPackage::new("pkg2", "/project2/package.json");
        let mut finder = MockProjectFinder::new()
            .with_package(package1)
            .with_package(package2);

        // Check a file in project1 only
        finder
            .check_changed(Path::new("/project1/src/index.js"))
            .unwrap();

        // Only project1 should be changed
        assert!(finder.projects()[0].is_changed());
        assert!(!finder.projects()[1].is_changed());
    }

    #[test]
    fn test_project_finder_with_workspace() {
        let workspace = MockWorkspace::new("root", "/project/package.json");
        let mut finder = MockProjectFinder::new().with_workspace(workspace);

        finder
            .check_changed(Path::new("/project/src/index.js"))
            .unwrap();

        assert!(finder.projects()[0].is_changed());
    }

    #[tokio::test]
    async fn test_project_finder_finalize() {
        let mut finder = MockProjectFinder::new();
        let result = finder.finalize().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_project_finder_finalize_with_projects() {
        let package = MockPackage::new("pkg1", "/project/package.json");
        let mut finder = MockProjectFinder::new().with_package(package);
        let result = finder.finalize().await;
        assert!(result.is_ok());
        assert_eq!(finder.projects().len(), 1);
    }
}
