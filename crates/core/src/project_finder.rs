use std::path::Path;

use crate::project::Project;
use anyhow::Result;
use async_trait::async_trait;

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
    /// # Errors
    /// Returns error if checking changed status fails for any project.
    fn check_changed(&mut self, path: &Path) -> Result<()> {
        for project in self.projects_mut() {
            project.check_changed(path)?;
        }
        Ok(())
    }
    async fn test(&self) -> Result<()> {
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

    #[tokio::test]
    async fn test_project_finder_test() {
        let finder = MockProjectFinder::new();
        let result = finder.test().await;
        assert!(result.is_ok());
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
}
