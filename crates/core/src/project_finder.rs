use std::path::{Path, PathBuf};

use crate::project::Project;
use anyhow::{Context, Result};
use async_trait::async_trait;

/// Generates `projects()`, `projects_mut()`, `project_count()`, and
/// `extend_projects()` for finders backed by a
/// `projects: HashMap<PathBuf, Project>` field.
///
/// The `extend_projects` body drains `self.projects.values()` straight into
/// the caller's buffer, so the intermediate `Vec<&Project>` that `projects()`
/// has to materialize is never built. Yield order is `HashMap::values()` in
/// both bodies, so overriding is order-preserving with respect to the
/// defaulted [`ProjectFinder::extend_projects`].
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
        fn extend_projects<'a>(&'a self, out: &mut ::std::vec::Vec<&'a $crate::Project>) {
            out.extend(self.projects.values());
        }
    };
}

/// Generates `dependencies()` and `add_dependency()` for types with a
/// `dependencies: HashSet<String>` field.
///
/// `add_dependency` probes membership with `contains` before allocating.
/// `HashSet<String>` borrows its keys as `str`, so the probe is
/// allocation-free, while the previous unconditional
/// `insert(dependency.to_string())` heap-allocated a fresh `String` that
/// `insert` immediately dropped whenever the name was already present.
/// Callers hit that duplicate path routinely: a manifest that lists the same
/// package in more than one dependency section (e.g. `dependencies` and
/// `peerDependencies` in `package.json`) is walked section by section, and
/// every section after the first re-adds a name already in the set. The set
/// contents are unchanged either way — a `HashSet` keeps its existing key on
/// a duplicate insert — so this is purely allocation elision.
#[macro_export]
macro_rules! impl_dependencies_accessors {
    () => {
        fn dependencies(&self) -> &::std::collections::HashSet<::std::string::String> {
            &self.dependencies
        }
        fn add_dependency(&mut self, dependency: &str) {
            if !self.dependencies.contains(dependency) {
                self.dependencies
                    .insert(::std::string::ToString::to_string(dependency));
            }
        }
    };
}

/// Generates `is_publishable_by_default()` for package/workspace structs with a
/// `publishable_by_default: bool` field.
///
/// Contract: the implementing struct MUST own a field named exactly
/// `publishable_by_default` of type `bool` — the same field
/// [`impl_discovered_new!`] initializes. Language crates whose publishability
/// is derived from a differently named field (e.g. Java's `has_publish_task`)
/// must keep their hand-rolled body instead.
#[macro_export]
macro_rules! impl_publishable_by_default {
    () => {
        fn is_publishable_by_default(&self) -> ::std::primitive::bool {
            self.publishable_by_default
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
    // lives in `CSharpPackage::dry_run_publish` (see
    // `crates/csharp/src/dry_run.rs::resolve_and_run_dry_run`), which
    // honors `config.publishDryRun` overrides first and falls back to a
    // managed `dotnet pack` + `dotnet nuget push` against ephemeral
    // `tempfile::TempDir` directories when no override is set.
    ($publish:path) => {
        fn default_publish_command(&self) -> ::std::string::String {
            $publish.to_string()
        }
        fn default_dry_run_publish_command(&self) -> ::std::option::Option<::std::string::String> {
            ::std::option::Option::None
        }
    };
}

/// Generates the `get_publish_command` / `get_dry_run_publish_command`
/// trait defaults shared by [`Package`](crate::Package) and
/// [`Workspace`](crate::Workspace).
///
/// Both traits resolve their publish commands through the exact same
/// [`crate::publish`] ladder — only the surrounding doc prose used to
/// differ — so the bodies live here once instead of being kept
/// byte-identical by hand in two files.
///
/// Contract: the invoking trait MUST already declare `relative_path()`,
/// `language()`, `default_publish_command()`, and
/// `default_dry_run_publish_command()`.
///
/// The sibling `publish` / `dry_run_publish` defaults are deliberately NOT
/// generated here: they differ by the `PACKAGE_DIR_NOT_FOUND` vs
/// `WORKSPACE_DIR_NOT_FOUND` message constant.
#[macro_export]
macro_rules! impl_publish_command_resolvers {
    () => {
        /// Get the publish command for this project, checking config first.
        ///
        /// The `default_publish_command()` closure is `FnOnce`, so the
        /// project's language-specific default (e.g. Node's
        /// `detect_package_manager_recursive`, which walks the ancestor chain
        /// with sync filesystem stats) is only invoked when config supplies
        /// neither a per-path nor a per-language override — the common case
        /// where the user configures a custom publish command in
        /// `.changepacks/config.json` now avoids one `String` allocation and,
        /// for Node, the ancestor-walking probe.
        fn get_publish_command(&self, config: &$crate::Config) -> ::std::string::String {
            $crate::publish::resolve_publish_command(
                self.relative_path(),
                self.language(),
                || self.default_publish_command(),
                config,
            )
        }

        /// Get the dry-run publish command for this project, checking config
        /// first, then falling back to the project's
        /// `default_dry_run_publish_command`.
        ///
        /// Mirrors `get_publish_command` — the default closure is `FnOnce` so
        /// it is only invoked on the cache-miss path.
        fn get_dry_run_publish_command(
            &self,
            config: &$crate::Config,
        ) -> ::std::option::Option<::std::string::String> {
            $crate::publish::resolve_dry_run_publish_command(
                self.relative_path(),
                self.language(),
                || self.default_dry_run_publish_command(),
                config,
            )
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

/// Generates constructors for discovered package/workspace structs with a
/// `publishable_by_default` field.
#[macro_export]
macro_rules! impl_discovered_new {
    () => {
        #[must_use]
        pub fn new(
            name: ::std::option::Option<::std::string::String>,
            version: ::std::option::Option<::std::string::String>,
            path: ::std::path::PathBuf,
            relative_path: ::std::path::PathBuf,
        ) -> Self {
            Self::new_discovered(name, version, path, relative_path, true)
        }

        #[must_use]
        pub(crate) fn new_discovered(
            name: ::std::option::Option<::std::string::String>,
            version: ::std::option::Option<::std::string::String>,
            path: ::std::path::PathBuf,
            relative_path: ::std::path::PathBuf,
            publishable_by_default: ::std::primitive::bool,
        ) -> Self {
            Self {
                name,
                version,
                path,
                relative_path,
                is_changed: false,
                publishable_by_default,
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
    /// Number of projects held by this finder.
    ///
    /// Required rather than defaulted: a `self.projects().len()` default would
    /// allocate and immediately drop a whole `Vec<&Project>` just to read a
    /// length. Every implementor already owns an O(1), allocation-free count
    /// (the language finders get one from
    /// [`impl_projects_hashmap_accessors!`]), so the trait demands it instead
    /// of offering a lossy shortcut.
    fn project_count(&self) -> usize;
    /// Append every project held by this finder onto `out`.
    ///
    /// Exists for the same reason [`ProjectFinder::project_count`] does:
    /// callers that merge several finders into one buffer (the CLI's
    /// `collect_projects`) would otherwise pay one throwaway `Vec<&Project>`
    /// per finder — allocated by `projects()` and dropped one line later —
    /// on every `check`, `update`, `publish`, and default-changepack run.
    /// Pushing into the caller's buffer removes that per-finder allocation.
    ///
    /// The default body is the compatibility path for external implementors:
    /// it forwards to `projects()`, so an implementor that only supplies the
    /// required accessors keeps compiling and keeps identical behaviour, and
    /// merely forfeits the allocation elision. Implementors backed by a
    /// `HashMap<PathBuf, Project>` get the elided override for free from
    /// [`impl_projects_hashmap_accessors!`].
    ///
    /// Contract for overrides: append in exactly `projects()` order and never
    /// clear or reorder what `out` already holds — callers rely on the merged
    /// order for their output (e.g. `changepacks check`).
    fn extend_projects<'a>(&'a self, out: &mut Vec<&'a Project>) {
        out.extend(self.projects());
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
    use crate::{Package, Workspace};
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

    // `add_dependency` now probes `contains` before allocating. These cases
    // lock the observable contract the probe must not change: a repeated name
    // is still stored exactly once, and distinct names all still land.
    #[test]
    fn test_add_dependency_deduplicates_repeated_names() {
        let mut package = MockPackage::same_path("pkg", "/project/package.json");

        // The duplicate path: the same name arrives from two manifest sections.
        package.add_dependency("serde");
        package.add_dependency("serde");
        package.add_dependency("serde");

        assert_eq!(
            package.dependencies().len(),
            1,
            "repeated add_dependency must keep exactly one entry"
        );
        assert!(package.dependencies().contains("serde"));

        // Distinct names still insert normally (the miss path is unchanged).
        package.add_dependency("tokio");
        package.add_dependency("anyhow");
        package.add_dependency("tokio");

        let mut names = package.dependencies().iter().cloned().collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["anyhow", "serde", "tokio"]);
    }

    #[test]
    fn test_add_dependency_deduplicates_on_workspace_too() {
        // The macro backs both the Package and the Workspace impls of all six
        // language crates, so pin the behaviour at the Workspace surface as well.
        let mut workspace = MockWorkspace::same_path("root", "/project/package.json");

        workspace.add_dependency("left-pad");
        workspace.add_dependency("left-pad");

        assert_eq!(workspace.dependencies().len(), 1);
        assert!(workspace.dependencies().contains("left-pad"));
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

        fn project_count(&self) -> usize {
            self.projects.len()
        }

        fn project_files(&self) -> &[&str] {
            &["package.json"]
        }

        async fn visit(&mut self, _path: &Path, _relative_path: &Path) -> Result<()> {
            Ok(())
        }
    }

    /// HashMap-backed finder that takes its accessors from
    /// [`impl_projects_hashmap_accessors!`], so the macro's `extend_projects`
    /// override is exercised inside `core` (the six language finders use the
    /// exact same expansion).
    #[derive(Debug)]
    struct HashMapProjectFinder {
        projects: std::collections::HashMap<PathBuf, Project>,
    }

    impl HashMapProjectFinder {
        fn with_packages(names: &[(&str, &str)]) -> Self {
            let mut projects = std::collections::HashMap::new();
            for (name, path) in names {
                projects.insert(
                    PathBuf::from(*path),
                    Project::Package(Box::new(MockPackage::same_path(name, path))),
                );
            }
            Self { projects }
        }
    }

    #[async_trait]
    impl ProjectFinder for HashMapProjectFinder {
        crate::impl_projects_hashmap_accessors!();

        fn project_files(&self) -> &[&str] {
            &["package.json"]
        }

        async fn visit(&mut self, _path: &Path, _relative_path: &Path) -> Result<()> {
            Ok(())
        }
    }

    fn project_names(projects: &[&Project]) -> Vec<String> {
        projects
            .iter()
            .map(|project| project.name().unwrap_or_default().to_string())
            .collect()
    }

    // The defaulted `extend_projects` body is the compatibility path for
    // external implementors: `MockProjectFinder` does NOT override it, so this
    // pins that the default appends exactly `projects()`, in `projects()`
    // order, without disturbing what the buffer already holds.
    #[test]
    fn test_extend_projects_default_matches_projects_and_preserves_buffer() {
        let finder = MockProjectFinder::new()
            .with_package(MockPackage::same_path("pkg1", "/project1/package.json"))
            .with_workspace(MockWorkspace::same_path("root", "/project2/package.json"))
            .with_package(MockPackage::same_path("pkg2", "/project3/package.json"));

        let seed = MockProjectFinder::new()
            .with_package(MockPackage::same_path("seed", "/seed/package.json"));
        let mut out = seed.projects();
        finder.extend_projects(&mut out);

        let mut expected = vec!["seed".to_string()];
        expected.extend(project_names(&finder.projects()));
        assert_eq!(project_names(&out), expected);
    }

    #[test]
    fn test_extend_projects_default_on_empty_finder_is_a_no_op() {
        let finder = MockProjectFinder::new();
        let mut out: Vec<&Project> = Vec::new();
        finder.extend_projects(&mut out);
        assert!(out.is_empty());
    }

    // The macro override skips the intermediate Vec that `projects()` builds;
    // both must still yield the same projects in the same `HashMap::values()`
    // order, so a caller can swap one for the other without reordering output.
    #[test]
    fn test_extend_projects_macro_override_matches_projects_order() {
        let finder = HashMapProjectFinder::with_packages(&[
            ("pkg1", "/project1/package.json"),
            ("pkg2", "/project2/package.json"),
            ("pkg3", "/project3/package.json"),
        ]);

        let mut out: Vec<&Project> = Vec::new();
        finder.extend_projects(&mut out);

        assert_eq!(out.len(), finder.project_count());
        assert_eq!(project_names(&out), project_names(&finder.projects()));
    }

    #[test]
    fn test_extend_projects_macro_override_preserves_existing_buffer_contents() {
        let first = HashMapProjectFinder::with_packages(&[("pkg1", "/project1/package.json")]);
        let second = HashMapProjectFinder::with_packages(&[("pkg2", "/project2/package.json")]);

        // Mirrors the CLI's `collect_projects`: one buffer, several finders.
        let mut out: Vec<&Project> = Vec::new();
        first.extend_projects(&mut out);
        second.extend_projects(&mut out);

        assert_eq!(project_names(&out), vec!["pkg1", "pkg2"]);
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

    // `matches_project_file` is the defaulted gate every non-CSharp language
    // finder calls before parsing a manifest. `MockProjectFinder::project_files`
    // returns exactly `["package.json"]`, so these cases pin all four exits of
    // its documented name-first / stat-last order.

    // Exit 4 (the only `true`): recognized name AND a real regular file.
    #[tokio::test]
    async fn test_matches_project_file_accepts_recognized_regular_file() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let manifest = temp_dir.path().join("package.json");
        std::fs::write(&manifest, "{}").unwrap();

        let finder = MockProjectFinder::new();
        assert!(
            finder.matches_project_file(&manifest).await.unwrap(),
            "a real file named package.json must be recognized"
        );
    }

    // Exit 4 again, negative half: the name matches but the entry is a
    // DIRECTORY, so the stat must veto it. This is why the stat cannot simply
    // be dropped once the name check is in place.
    #[tokio::test]
    async fn test_matches_project_file_rejects_directory_with_recognized_name() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("package.json");
        std::fs::create_dir(&dir_path).unwrap();

        let finder = MockProjectFinder::new();
        assert!(
            !finder.matches_project_file(&dir_path).await.unwrap(),
            "a directory named package.json must not be treated as a manifest"
        );
    }

    // Exit 3: an unrecognized name is rejected even though the file really
    // exists — the name guard, not the stat, is what filters it out.
    #[tokio::test]
    async fn test_matches_project_file_rejects_unrecognized_name() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let other = temp_dir.path().join("Cargo.toml");
        std::fs::write(&other, "[package]\n").unwrap();

        let finder = MockProjectFinder::new();
        assert!(
            !finder.matches_project_file(&other).await.unwrap(),
            "Cargo.toml is not in this finder's project_files()"
        );
    }

    // Exit 1: `file_name()` is `None` for a path ending in `..`, even though
    // that path resolves to an existing directory. The early return must fire
    // before any stat.
    #[tokio::test]
    async fn test_matches_project_file_rejects_path_without_file_name() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let parent_ref = temp_dir.path().join("..");
        assert!(parent_ref.file_name().is_none());

        let finder = MockProjectFinder::new();
        assert!(
            !finder.matches_project_file(&parent_ref).await.unwrap(),
            "a path with no file name cannot match a manifest name"
        );
    }

    // Exit 2: `to_str()` is `None` for a non-UTF-8 file name. Such a name
    // cannot equal any ASCII manifest name, so the guard must short-circuit to
    // `Ok(false)` before any stat — exactly what the method doc reasons about.
    // The path deliberately does not exist: the early return fires first.
    #[tokio::test]
    async fn test_matches_project_file_rejects_non_utf8_file_name() {
        #[cfg(unix)]
        let name: std::ffi::OsString = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(b"\xFF\xFEpackage.json").to_os_string()
        };
        #[cfg(windows)]
        let name: std::ffi::OsString = {
            use std::os::windows::ffi::OsStringExt;
            // Unpaired high surrogate — unrepresentable in UTF-8.
            std::ffi::OsString::from_wide(&[0xD800, u16::from(b'x')])
        };

        // Stay honest if a platform ever normalizes the name away.
        assert!(
            Path::new(&name)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_none(),
            "fixture must really be a non-UTF-8 file name"
        );

        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join(&name);

        let finder = MockProjectFinder::new();
        assert!(
            !finder.matches_project_file(&path).await.unwrap(),
            "a non-UTF-8 file name cannot match an ASCII manifest name"
        );
    }

    // Recognized name, nothing on disk: `is_regular_file` maps NotFound to
    // `Ok(false)` rather than an error, so the gate stays quiet for deleted
    // manifests still listed in the git index.
    #[tokio::test]
    async fn test_matches_project_file_rejects_missing_manifest() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let missing = temp_dir.path().join("package.json");

        let finder = MockProjectFinder::new();
        assert!(
            !finder.matches_project_file(&missing).await.unwrap(),
            "a package.json that does not exist must not match"
        );
    }
}
