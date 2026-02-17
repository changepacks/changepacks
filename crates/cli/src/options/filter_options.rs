use changepacks_core::Project;
use clap::ValueEnum;

/// CLI filter for workspace-only or package-only listing.
///
/// Used by the check command to filter projects by type.
#[derive(Debug, Clone, ValueEnum)]
pub enum FilterOptions {
    /// Show only workspace projects
    Workspace,
    /// Show only package projects
    Package,
}

impl FilterOptions {
    #[must_use]
    pub fn matches(&self, project: &Project) -> bool {
        match self {
            Self::Workspace => matches!(project, Project::Workspace(_)),
            Self::Package => matches!(project, Project::Package(_)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use changepacks_core::{Language, Package, UpdateType, Workspace};
    use clap::ValueEnum;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    #[derive(Debug)]
    struct MockPackage {
        name: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        version: Option<String>,
        language: Language,
        dependencies: HashSet<String>,
        changed: bool,
    }

    #[async_trait]
    impl Package for MockPackage {
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }

        fn version(&self) -> Option<&str> {
            self.version.as_deref()
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn relative_path(&self) -> &Path {
            &self.relative_path
        }

        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_changed(&self) -> bool {
            self.changed
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

        fn set_changed(&mut self, changed: bool) {
            self.changed = changed;
        }

        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
    }

    #[derive(Debug)]
    struct MockWorkspace {
        name: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        version: Option<String>,
        language: Language,
        dependencies: HashSet<String>,
        changed: bool,
    }

    #[async_trait]
    impl Workspace for MockWorkspace {
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }

        fn version(&self) -> Option<&str> {
            self.version.as_deref()
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn relative_path(&self) -> &Path {
            &self.relative_path
        }

        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_changed(&self) -> bool {
            self.changed
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

        fn set_changed(&mut self, changed: bool) {
            self.changed = changed;
        }

        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }

        async fn update_workspace_dependencies(
            &self,
            _packages: &[&dyn Package],
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn workspace_project() -> Project {
        Project::Workspace(Box::new(MockWorkspace {
            name: Some("workspace".to_string()),
            path: PathBuf::from("/repo/package.json"),
            relative_path: PathBuf::from("package.json"),
            version: Some("1.0.0".to_string()),
            language: Language::Node,
            dependencies: HashSet::new(),
            changed: false,
        }))
    }

    fn package_project() -> Project {
        Project::Package(Box::new(MockPackage {
            name: Some("package".to_string()),
            path: PathBuf::from("/repo/crates/pkg/Cargo.toml"),
            relative_path: PathBuf::from("crates/pkg/Cargo.toml"),
            version: Some("1.0.0".to_string()),
            language: Language::Rust,
            dependencies: HashSet::new(),
            changed: false,
        }))
    }

    #[test]
    fn test_filter_options_matches_workspace_with_workspace_project() {
        let project = workspace_project();
        assert!(FilterOptions::Workspace.matches(&project));
    }

    #[test]
    fn test_filter_options_matches_workspace_with_package_project() {
        let project = package_project();
        assert!(!FilterOptions::Workspace.matches(&project));
    }

    #[test]
    fn test_filter_options_matches_package_with_package_project() {
        let project = package_project();
        assert!(FilterOptions::Package.matches(&project));
    }

    #[test]
    fn test_filter_options_matches_package_with_workspace_project() {
        let project = workspace_project();
        assert!(!FilterOptions::Package.matches(&project));
    }

    #[test]
    fn test_filter_options_value_enum_workspace() {
        let filter = FilterOptions::from_str("workspace", true).unwrap();
        assert!(matches!(filter, FilterOptions::Workspace));
    }

    #[test]
    fn test_filter_options_value_enum_package() {
        let filter = FilterOptions::from_str("package", true).unwrap();
        assert!(matches!(filter, FilterOptions::Package));
    }
}
