use std::path::Path;

use crate::project::Project;
use anyhow::{Context, Result};
use async_trait::async_trait;

/// Expand to the identical `projects()` / `projects_mut()` accessor pair
/// used by every finder whose backing store is a `HashMap<PathBuf,
/// Project>` (Node, Python, Dart share byte-identical bodies today).
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
/// Rust / CSharp / Java finders are intentionally NOT consumers:
/// - RustProjectFinder's projects live under a `projects_by_id`
///   IndexMap, not the flat `projects` HashMap this macro assumes.
/// - CSharpProjectFinder also carries the `is_workspace_cache` field
///   introduced in retry-now#0029 and uses `.csproj` extension matching.
/// - JavaProjectFinder shells out to gradlew and stores state
///   differently.
///
/// Adding a new consumer requires only that its struct has a
/// `projects: HashMap<PathBuf, Project>` field with those exact
/// spellings.
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
    /// # Errors
    /// Returns error if the path has no file name component or the file name
    /// is not valid UTF-8.
    async fn matches_project_file(&self, path: &Path) -> Result<bool> {
        if !is_regular_file(path).await {
            return Ok(false);
        }
        let name = path
            .file_name()
            .with_context(|| format!("File name not found - {}", path.display()))?
            .to_str()
            .with_context(|| format!("File name not found - {}", path.display()))?;
        Ok(self.project_files().contains(&name))
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
        path: PathBuf,
        relative_path: PathBuf,
        changed: bool,
        dependencies: HashSet<String>,
    }

    impl MockPackage {
        fn new(name: &str, path: &str) -> Self {
            Self {
                name: Some(name.to_string()),
                path: PathBuf::from(path),
                relative_path: PathBuf::from(path),
                changed: false,
                dependencies: HashSet::new(),
            }
        }
    }

    #[async_trait]
    impl Package for MockPackage {
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }
        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }
        fn path(&self) -> &Path {
            &self.path
        }
        fn relative_path(&self) -> &Path {
            &self.relative_path
        }
        async fn update_version(&mut self, _update_type: UpdateType) -> Result<()> {
            Ok(())
        }
        fn is_changed(&self) -> bool {
            self.changed
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
        fn set_changed(&mut self, changed: bool) {
            self.changed = changed;
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
        path: PathBuf,
        relative_path: PathBuf,
        changed: bool,
        dependencies: HashSet<String>,
    }

    impl MockWorkspace {
        fn new(name: &str, path: &str) -> Self {
            Self {
                name: Some(name.to_string()),
                path: PathBuf::from(path),
                relative_path: PathBuf::from(path),
                changed: false,
                dependencies: HashSet::new(),
            }
        }
    }

    #[async_trait]
    impl Workspace for MockWorkspace {
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }
        fn path(&self) -> &Path {
            &self.path
        }
        fn relative_path(&self) -> &Path {
            &self.relative_path
        }
        fn version(&self) -> Option<&str> {
            Some("1.0.0")
        }
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
        fn is_changed(&self) -> bool {
            self.changed
        }
        fn set_changed(&mut self, changed: bool) {
            self.changed = changed;
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
