use std::path::{Path, PathBuf};

use crate::project::Project;
use anyhow::{Context, Result};
use async_trait::async_trait;

/// Generates `projects()`, `projects_mut()`, and `project_count()` for finders backed by a
/// `projects: HashMap<PathBuf, Project>` field.
#[macro_export]
macro_rules! impl_projects_hashmap_accessors {
    () => {
        fn projects(&self) -> ::std::vec::Vec<&$crate::Project> {
            self.projects.values().collect::<::std::vec::Vec<_>>()
        }
        fn projects_mut(&mut self) -> ::std::vec::Vec<&mut $crate::Project> {
            self.projects.values_mut().collect::<::std::vec::Vec<_>>()
        }
        fn project_count(&self) -> ::std::primitive::usize {
            self.projects.len()
        }
    };
}

/// Generates `dependencies()` and `add_dependency()` for types with a
/// `dependencies: HashSet<String>` field.
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

/// Generates const-backed publish command defaults.
///
/// Two arguments return `Some($dry_run.to_string())`; one argument returns
/// `None` for ecosystems without a built-in dry-run command.
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

/// Generates the shared basic accessors for package/workspace structs with
/// `name`, `version`, `path`, `relative_path`, and `is_changed` fields.
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

/// Generates the standard constructor for package/workspace structs with
/// `name`, `version`, `path`, `relative_path`, `is_changed`, and
/// `dependencies` fields.
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

/// Generates `fn language(&self) -> Language` for a fixed language variant.
#[macro_export]
macro_rules! impl_language {
    ($lang:expr) => {
        fn language(&self) -> $crate::Language {
            $lang
        }
    };
}

/// Returns `true` when `path`'s extension matches `ext` case-insensitively
/// (ASCII only).
///
/// Mirrors the `path.extension().and_then(|e| e.to_str()).is_some_and(|e|
/// e.eq_ignore_ascii_case(ext))` idiom used across language crates so the
/// predicate lives in exactly one place. Returns `false` when the path has
/// no extension (including dotfiles such as `.json`, where
/// [`std::path::Path::extension`] returns `None`).
///
/// Public so cross-crate callers (e.g. `changepacks-csharp`,
/// `changepacks-java`, `changepacks-utils`) can reuse it via the re-export
/// from `changepacks_core::lib.rs`.
pub fn has_extension_ignore_ascii_case(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Returns `Ok(true)` when `path` refers to an existing regular file.
///
/// AGENTS.md rule: never blocking I/O in async — use `tokio::fs::metadata`.
/// A missing path or directory returns `Ok(false)`. Other metadata errors are
/// propagated with the failing path in their context.
///
/// Shared between `ProjectFinder::matches_project_file` (name-based match
/// used by every language) and `CSharpProjectFinder::visit` (extension-based
/// match) so the byte-identical stat + `is_file()` fallthrough lives in ONE
/// place. Public so cross-crate callers (e.g. `changepacks-csharp`) can
/// reuse it via the re-export from `changepacks_core::lib.rs`.
pub async fn is_regular_file(path: &Path) -> Result<bool> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to read metadata for {}", path.display()))
        }
    }
}

/// Visitor pattern for discovering projects by walking the git tree.
///
/// Each language implements this trait to detect its project files (package.json, Cargo.toml, etc.)
/// and build a collection of projects. The `visit` method is called for each file in the git tree.
#[async_trait]
pub trait ProjectFinder: std::fmt::Debug + Send + Sync {
    fn projects(&self) -> Vec<&Project>;
    fn projects_mut(&mut self) -> Vec<&mut Project>;
    fn project_count(&self) -> usize {
        self.projects().len()
    }
    fn project_files(&self) -> &[&str];
    /// # Errors
    /// Returns error if the file visitation fails.
    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()>;
    /// Whether `path` is a project manifest file recognized by this finder.
    ///
    /// Returns `false` for directories and files whose name is not in
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
    /// Returns an error when metadata for a recognized manifest path cannot be
    /// read for a reason other than the path not existing.
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
        is_regular_file(path).await
    }
    /// Mark every project against every path in `paths` from ONE
    /// `projects_mut()` call.
    ///
    /// The driver dispatches every changed file to every finder. Rebuilding
    /// the `Vec<&mut Project>` via `projects_mut()` once per file would cost
    /// `F` changed files × `M` finders fresh Vec allocations. Collecting the
    /// paths once and looping project-major here collapses that to one Vec per
    /// finder (`M` total).
    ///
    /// The project-major / path-major order flip is behavior-preserving:
    /// [`Project::check_changed`] is monotonic — it early-returns once the
    /// project is already changed and only ever sets `changed = true` via the
    /// pure, stateless `should_mark_changed`. A project ends up changed iff
    /// *any* path matches, an order-independent logical OR, so visiting all
    /// paths for one project before moving to the next yields an identical
    /// result to a path-major traversal.
    ///
    /// # Errors
    /// Returns error if checking changed status fails for any project.
    fn check_changed_many(&mut self, paths: &[PathBuf]) -> Result<()> {
        for project in self.projects_mut() {
            for path in paths {
                project.check_changed(path)?;
                // Early break: check_changed is monotonic, so once changed, remaining paths are redundant.
                if project.is_changed() {
                    break;
                }
            }
        }
        Ok(())
    }
    /// Post-visit processing hook for resolving deferred state (e.g., workspace-inherited versions).
    /// Called once after all `visit()` calls complete.
    /// # Errors
    /// Returns error if finalization fails.
    async fn finalize(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockPackage, MockWorkspace};
    use async_trait::async_trait;
    use rstest::rstest;
    use std::path::PathBuf;

    // `Path::new(".json").extension()` returns `None` in Rust — dotfiles have
    // no extension — so `has_extension_ignore_ascii_case(Path::new(".json"), "json")`
    // is `false`. This matches the behaviour of every call site that wraps a
    // bare filename with `Path::new(file_name)`.
    #[rstest]
    #[case("foo.json", "json", true)]
    #[case("foo.JSON", "json", true)]
    #[case("foo.Json", "json", true)]
    #[case("foo", "json", false)]
    #[case(".json", "json", false)]
    #[case("foo.jsonx", "json", false)]
    fn test_has_extension_ignore_ascii_case(
        #[case] file: &str,
        #[case] ext: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(
            has_extension_ignore_ascii_case(Path::new(file), ext),
            expected,
            "has_extension_ignore_ascii_case(Path::new({file:?}), {ext:?})"
        );
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
        let package = MockPackage::same_path("test", "/project/package.json");
        let mut finder = MockProjectFinder::new().with_package(package);

        // Check a file that's in the project directory
        finder
            .check_changed_many(&[PathBuf::from("/project/src/index.js")])
            .unwrap();

        // The project should be marked as changed
        assert!(finder.projects()[0].is_changed());
    }

    #[test]
    fn test_project_finder_check_changed_multiple_projects() {
        let package1 = MockPackage::same_path("pkg1", "/project1/package.json");
        let package2 = MockPackage::same_path("pkg2", "/project2/package.json");
        let mut finder = MockProjectFinder::new()
            .with_package(package1)
            .with_package(package2);

        // Check a file in project1 only
        finder
            .check_changed_many(&[PathBuf::from("/project1/src/index.js")])
            .unwrap();

        // Only project1 should be changed
        assert!(finder.projects()[0].is_changed());
        assert!(!finder.projects()[1].is_changed());
    }

    #[test]
    fn test_project_finder_check_changed_many() {
        let package1 = MockPackage::same_path("pkg1", "/project1/package.json");
        let package2 = MockPackage::same_path("pkg2", "/project2/package.json");
        let workspace = MockWorkspace::same_path("root", "/project3/package.json");
        let mut finder = MockProjectFinder::new()
            .with_package(package1)
            .with_package(package2)
            .with_workspace(workspace);

        // One batch: a file under project1 and a file under project3 (the
        // workspace); nothing under project2. `check_changed_many` must mark
        // exactly project1 and project3 — a project is marked changed iff any
        // path matches it, proving the project-major loop order is
        // behavior-preserving across both Package and Workspace variants.
        let paths = [
            PathBuf::from("/project1/src/index.js"),
            PathBuf::from("/project3/lib/mod.rs"),
        ];
        finder.check_changed_many(&paths).unwrap();

        assert!(finder.projects()[0].is_changed());
        assert!(!finder.projects()[1].is_changed());
        assert!(finder.projects()[2].is_changed());
    }

    #[test]
    fn test_project_finder_check_changed_many_matches_per_file_traversal() {
        // The same inputs fed one-at-a-time (each path its own single-element
        // batch, mirroring a per-file traversal) and fed together in ONE batch
        // must land the two finders in an identical changed-state, locking the
        // order/batch equivalence the driver relies on.
        let paths = [
            PathBuf::from("/project1/src/index.js"),
            PathBuf::from("/project2/README.md"),
        ];

        let mut per_path = MockProjectFinder::new()
            .with_package(MockPackage::same_path("pkg1", "/project1/package.json"))
            .with_package(MockPackage::same_path("pkg2", "/project2/package.json"));
        for path in &paths {
            per_path
                .check_changed_many(std::slice::from_ref(path))
                .unwrap();
        }

        let mut batched = MockProjectFinder::new()
            .with_package(MockPackage::same_path("pkg1", "/project1/package.json"))
            .with_package(MockPackage::same_path("pkg2", "/project2/package.json"));
        batched.check_changed_many(&paths).unwrap();

        assert_eq!(
            per_path.projects()[0].is_changed(),
            batched.projects()[0].is_changed()
        );
        assert_eq!(
            per_path.projects()[1].is_changed(),
            batched.projects()[1].is_changed()
        );
        assert!(batched.projects()[0].is_changed());
        assert!(batched.projects()[1].is_changed());
    }

    #[test]
    fn test_project_finder_with_workspace() {
        let workspace = MockWorkspace::same_path("root", "/project/package.json");
        let mut finder = MockProjectFinder::new().with_workspace(workspace);

        finder
            .check_changed_many(&[PathBuf::from("/project/src/index.js")])
            .unwrap();

        assert!(finder.projects()[0].is_changed());
    }

    #[tokio::test]
    async fn test_default_project_finder_finalize_is_covered_no_op() {
        assert!(
            !include_str!("project_finder.rs")
                .contains(concat!("#[cfg(not(", "tarpaulin_include))]"))
        );

        let mut finder = MockProjectFinder::new();
        let result = finder.finalize().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_is_regular_file_with_existing_file() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "test content").unwrap();

        let result = is_regular_file(&file_path).await;
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn test_is_regular_file_with_directory() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("subdir");
        std::fs::create_dir(&dir_path).unwrap();

        let result = is_regular_file(&dir_path).await;
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_is_regular_file_with_missing_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let missing_path = temp_dir.path().join("nonexistent.txt");

        let result = is_regular_file(&missing_path).await;
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_is_regular_file_propagates_metadata_error_with_path_context() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        #[cfg(windows)]
        let invalid_path = temp_dir.path().join("invalid\0path");
        #[cfg(unix)]
        let invalid_path = {
            use std::os::unix::fs::symlink;

            let path = temp_dir.path().join("metadata-loop");
            symlink(&path, &path).unwrap();
            path
        };

        let error = is_regular_file(&invalid_path)
            .await
            .expect_err("metadata errors other than NotFound must be propagated");
        let chain = format!("{error:#}");
        assert!(
            chain.contains(&invalid_path.display().to_string()),
            "error chain should name the path whose metadata failed, got: {chain}"
        );
    }
}
