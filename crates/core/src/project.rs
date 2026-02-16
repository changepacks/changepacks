use std::{
    cmp::Ordering,
    collections::HashSet,
    fmt::{Debug, Display},
    path::Path,
};

use anyhow::Result;
use colored::Colorize;

use crate::{config::Config, package::Package, update_type::UpdateType, workspace::Workspace};

#[derive(Debug)]
pub enum Project {
    Workspace(Box<dyn Workspace>),
    Package(Box<dyn Package>),
}

impl Project {
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Workspace(workspace) => workspace.name(),
            Self::Package(package) => package.name(),
        }
    }

    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Workspace(workspace) => workspace.version(),
            Self::Package(package) => package.version(),
        }
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Workspace(workspace) => workspace.path(),
            Self::Package(package) => package.path(),
        }
    }

    #[must_use]
    pub fn relative_path(&self) -> &Path {
        match self {
            Self::Workspace(workspace) => workspace.relative_path(),
            Self::Package(package) => package.relative_path(),
        }
    }

    /// # Errors
    /// Returns error if the underlying `update_version` call fails.
    pub async fn update_version(&mut self, update_type: UpdateType) -> Result<()> {
        match self {
            Self::Workspace(workspace) => workspace.update_version(update_type).await?,
            Self::Package(package) => package.update_version(update_type).await?,
        }
        Ok(())
    }

    /// # Errors
    /// Returns error if the underlying `check_changed` call fails.
    pub fn check_changed(&mut self, path: &Path) -> Result<()> {
        match self {
            Self::Workspace(workspace) => workspace.check_changed(path)?,
            Self::Package(package) => package.check_changed(path)?,
        }
        Ok(())
    }

    #[must_use]
    pub fn is_changed(&self) -> bool {
        match self {
            Self::Workspace(workspace) => workspace.is_changed(),
            Self::Package(package) => package.is_changed(),
        }
    }

    #[must_use]
    pub fn dependencies(&self) -> &HashSet<String> {
        match self {
            Self::Workspace(workspace) => workspace.dependencies(),
            Self::Package(package) => package.dependencies(),
        }
    }

    pub fn add_dependency(&mut self, dependency: &str) {
        match self {
            Self::Workspace(workspace) => workspace.add_dependency(dependency),
            Self::Package(package) => package.add_dependency(dependency),
        }
    }

    #[must_use]
    pub fn language(&self) -> crate::Language {
        match self {
            Self::Workspace(workspace) => workspace.language(),
            Self::Package(package) => package.language(),
        }
    }

    /// # Errors
    /// Returns error if the underlying publish call fails.
    pub async fn publish(&self, config: &Config) -> Result<()> {
        match self {
            Self::Workspace(workspace) => workspace.publish(config).await,
            Self::Package(package) => package.publish(config).await,
        }
    }
}

impl PartialEq for Project {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Project {}

impl PartialOrd for Project {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Project {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Workspace(_), Self::Package(_)) => Ordering::Less,
            (Self::Package(_), Self::Workspace(_)) => Ordering::Greater,
            (Self::Workspace(w1), Self::Workspace(w2)) => {
                let lang_ord = w1.language().cmp(&w2.language());
                if lang_ord != Ordering::Equal {
                    return lang_ord;
                }

                let name1 = w1.name();
                let name2 = w2.name();

                match (name1, name2) {
                    (Some(n1), Some(n2)) => n1.cmp(n2),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => {
                        let v1 = w1.version().unwrap_or("");
                        let v2 = w2.version().unwrap_or("");
                        v1.cmp(v2)
                    }
                }
            }
            (Self::Package(p1), Self::Package(p2)) => {
                let lang_ord = p1.language().cmp(&p2.language());
                if lang_ord != Ordering::Equal {
                    return lang_ord;
                }
                match (p1.name(), p2.name()) {
                    (Some(n1), Some(n2)) => n1.cmp(n2),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                }
            }
        }
    }
}

impl Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace(workspace) => {
                write!(
                    f,
                    "{} {} {} {} {}",
                    format!("[Workspace - {}]", workspace.language())
                        .bright_blue()
                        .bold(),
                    workspace.name().unwrap_or("noname").bright_white().bold(),
                    format!(
                        "({})",
                        workspace
                            .version()
                            .map_or("unknown".to_string(), |v| format!("v{v}")),
                    )
                    .bright_green(),
                    "-".bright_cyan(),
                    workspace
                        .relative_path()
                        .display()
                        .to_string()
                        .bright_black()
                )
            }
            Self::Package(package) => {
                write!(
                    f,
                    "{} {} {} {} {}",
                    format!("[{}]", package.language()).bright_blue().bold(),
                    package.name().unwrap_or("noname").bright_white().bold(),
                    format!(
                        "({})",
                        package
                            .version()
                            .map_or("unknown".to_string(), |v| format!("v{v}"))
                    )
                    .bright_green(),
                    "-".bright_cyan(),
                    package.relative_path().display().to_string().bright_black()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Language;
    use async_trait::async_trait;
    use std::path::PathBuf;

    #[derive(Debug)]
    struct MockWorkspace {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        dependencies: HashSet<String>,
        changed: bool,
    }

    impl MockWorkspace {
        fn new(name: Option<&str>, version: Option<&str>, language: Language) -> Self {
            Self {
                name: name.map(String::from),
                version: version.map(String::from),
                path: PathBuf::from("/test/package.json"),
                relative_path: PathBuf::from("package.json"),
                language,
                dependencies: HashSet::new(),
                changed: false,
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
            self.version.as_deref()
        }
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
        fn is_changed(&self) -> bool {
            self.changed
        }
        fn set_changed(&mut self, changed: bool) {
            self.changed = changed;
        }
        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
    }

    #[derive(Debug)]
    struct MockPackage {
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        language: Language,
        dependencies: HashSet<String>,
        changed: bool,
    }

    impl MockPackage {
        fn new(name: Option<&str>, version: Option<&str>, language: Language) -> Self {
            Self {
                name: name.map(String::from),
                version: version.map(String::from),
                path: PathBuf::from("/test/Cargo.toml"),
                relative_path: PathBuf::from("Cargo.toml"),
                language,
                dependencies: HashSet::new(),
                changed: false,
            }
        }
    }

    #[async_trait]
    impl Package for MockPackage {
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
            self.version.as_deref()
        }
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
        fn is_changed(&self) -> bool {
            self.changed
        }
        fn set_changed(&mut self, changed: bool) {
            self.changed = changed;
        }
        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
    }

    #[test]
    fn test_project_workspace_name() {
        let workspace = MockWorkspace::new(Some("test-ws"), Some("1.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        assert_eq!(project.name(), Some("test-ws"));
    }

    #[test]
    fn test_project_package_name() {
        let package = MockPackage::new(Some("test-pkg"), Some("1.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        assert_eq!(project.name(), Some("test-pkg"));
    }

    #[test]
    fn test_project_workspace_version() {
        let workspace = MockWorkspace::new(Some("test"), Some("2.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        assert_eq!(project.version(), Some("2.0.0"));
    }

    #[test]
    fn test_project_package_version() {
        let package = MockPackage::new(Some("test"), Some("3.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        assert_eq!(project.version(), Some("3.0.0"));
    }

    #[test]
    fn test_project_workspace_path() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        assert_eq!(project.path(), Path::new("/test/package.json"));
    }

    #[test]
    fn test_project_package_path() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        assert_eq!(project.path(), Path::new("/test/Cargo.toml"));
    }

    #[test]
    fn test_project_workspace_relative_path() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        assert_eq!(project.relative_path(), Path::new("package.json"));
    }

    #[test]
    fn test_project_package_relative_path() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        assert_eq!(project.relative_path(), Path::new("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_project_workspace_update_version() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let mut project = Project::Workspace(Box::new(workspace));
        let result = project.update_version(UpdateType::Minor).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_project_package_update_version() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let mut project = Project::Package(Box::new(package));
        let result = project.update_version(UpdateType::Patch).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_project_workspace_check_changed() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let mut project = Project::Workspace(Box::new(workspace));
        let result = project.check_changed(Path::new("/test/src/index.js"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_project_package_check_changed() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let mut project = Project::Package(Box::new(package));
        let result = project.check_changed(Path::new("/test/src/main.rs"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_project_workspace_is_changed() {
        let mut workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        workspace.changed = true;
        let project = Project::Workspace(Box::new(workspace));
        assert!(project.is_changed());
    }

    #[test]
    fn test_project_package_is_changed() {
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.changed = true;
        let project = Project::Package(Box::new(package));
        assert!(project.is_changed());
    }

    #[test]
    fn test_project_workspace_dependencies() {
        let mut workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        workspace.dependencies.insert("dep1".to_string());
        let project = Project::Workspace(Box::new(workspace));
        assert!(project.dependencies().contains("dep1"));
    }

    #[test]
    fn test_project_package_dependencies() {
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.dependencies.insert("dep2".to_string());
        let project = Project::Package(Box::new(package));
        assert!(project.dependencies().contains("dep2"));
    }

    #[test]
    fn test_project_workspace_add_dependency() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let mut project = Project::Workspace(Box::new(workspace));
        project.add_dependency("new-dep");
        assert!(project.dependencies().contains("new-dep"));
    }

    #[test]
    fn test_project_package_add_dependency() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let mut project = Project::Package(Box::new(package));
        project.add_dependency("new-dep");
        assert!(project.dependencies().contains("new-dep"));
    }

    #[test]
    fn test_project_workspace_language() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Python);
        let project = Project::Workspace(Box::new(workspace));
        assert!(matches!(project.language(), Language::Python));
    }

    #[test]
    fn test_project_package_language() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Dart);
        let project = Project::Package(Box::new(package));
        assert!(matches!(project.language(), Language::Dart));
    }

    #[tokio::test]
    async fn test_project_workspace_publish() {
        let temp_dir = std::env::temp_dir();
        let mut workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        workspace.path = temp_dir.join("package.json");
        let project = Project::Workspace(Box::new(workspace));
        let config = Config::default();
        let result = project.publish(&config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_project_package_publish() {
        let temp_dir = std::env::temp_dir();
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.path = temp_dir.join("Cargo.toml");
        let project = Project::Package(Box::new(package));
        let config = Config::default();
        let result = project.publish(&config).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_project_eq_same_workspace() {
        let w1 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert_eq!(p1, p2);
    }

    #[test]
    fn test_project_partial_ord() {
        let w1 = MockWorkspace::new(Some("a"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("b"), Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert!(p1.partial_cmp(&p2).is_some());
    }

    #[test]
    fn test_project_ord_workspace_before_package() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let p1 = Project::Workspace(Box::new(workspace));
        let p2 = Project::Package(Box::new(package));
        assert!(p1 < p2);
    }

    #[test]
    fn test_project_ord_package_after_workspace() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let p1 = Project::Package(Box::new(package));
        let p2 = Project::Workspace(Box::new(workspace));
        assert!(p1 > p2);
    }

    #[test]
    fn test_project_ord_workspaces_by_language() {
        let w1 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Python);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert_ne!(p1.cmp(&p2), Ordering::Equal);
    }

    #[test]
    fn test_project_ord_workspaces_by_name() {
        let w1 = MockWorkspace::new(Some("aaa"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("bbb"), Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert!(p1 < p2);
    }

    #[test]
    fn test_project_ord_workspaces_name_some_vs_none() {
        let w1 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(None, Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert!(p1 < p2);
    }

    #[test]
    fn test_project_ord_workspaces_name_none_vs_some() {
        let w1 = MockWorkspace::new(None, Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert!(p1 > p2);
    }

    #[test]
    fn test_project_ord_workspaces_both_none_names() {
        let w1 = MockWorkspace::new(None, Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(None, Some("2.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert!(p1 < p2);
    }

    #[test]
    fn test_project_ord_packages_by_language() {
        let pkg1 = MockPackage::new(Some("test"), Some("1.0.0"), Language::Node);
        let pkg2 = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let p1 = Project::Package(Box::new(pkg1));
        let p2 = Project::Package(Box::new(pkg2));
        assert_ne!(p1.cmp(&p2), Ordering::Equal);
    }

    #[test]
    fn test_project_ord_packages_by_name() {
        let pkg1 = MockPackage::new(Some("aaa"), Some("1.0.0"), Language::Rust);
        let pkg2 = MockPackage::new(Some("bbb"), Some("1.0.0"), Language::Rust);
        let p1 = Project::Package(Box::new(pkg1));
        let p2 = Project::Package(Box::new(pkg2));
        assert!(p1 < p2);
    }

    #[test]
    fn test_project_ord_packages_name_some_vs_none() {
        let pkg1 = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let pkg2 = MockPackage::new(None, Some("1.0.0"), Language::Rust);
        let p1 = Project::Package(Box::new(pkg1));
        let p2 = Project::Package(Box::new(pkg2));
        assert!(p1 < p2);
    }

    #[test]
    fn test_project_ord_packages_name_none_vs_some() {
        let pkg1 = MockPackage::new(None, Some("1.0.0"), Language::Rust);
        let pkg2 = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let p1 = Project::Package(Box::new(pkg1));
        let p2 = Project::Package(Box::new(pkg2));
        assert!(p1 > p2);
    }

    #[test]
    fn test_project_ord_packages_both_none_names() {
        let pkg1 = MockPackage::new(None, Some("1.0.0"), Language::Rust);
        let pkg2 = MockPackage::new(None, Some("1.0.0"), Language::Rust);
        let p1 = Project::Package(Box::new(pkg1));
        let p2 = Project::Package(Box::new(pkg2));
        assert_eq!(p1.cmp(&p2), Ordering::Equal);
    }

    #[test]
    fn test_project_display_workspace() {
        let workspace = MockWorkspace::new(Some("my-workspace"), Some("1.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        let display = format!("{}", project);
        assert!(display.contains("Workspace"));
        assert!(display.contains("my-workspace"));
        assert!(display.contains("v1.0.0"));
    }

    #[test]
    fn test_project_display_workspace_no_name() {
        let workspace = MockWorkspace::new(None, Some("1.0.0"), Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        let display = format!("{}", project);
        assert!(display.contains("noname"));
    }

    #[test]
    fn test_project_display_workspace_no_version() {
        let workspace = MockWorkspace::new(Some("test"), None, Language::Node);
        let project = Project::Workspace(Box::new(workspace));
        let display = format!("{}", project);
        assert!(display.contains("unknown"));
    }

    #[test]
    fn test_project_display_package() {
        let package = MockPackage::new(Some("my-package"), Some("2.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        let display = format!("{}", project);
        assert!(display.contains("my-package"));
        assert!(display.contains("v2.0.0"));
    }

    #[test]
    fn test_project_display_package_no_name() {
        let package = MockPackage::new(None, Some("1.0.0"), Language::Rust);
        let project = Project::Package(Box::new(package));
        let display = format!("{}", project);
        assert!(display.contains("noname"));
    }

    #[test]
    fn test_project_display_package_no_version() {
        let package = MockPackage::new(Some("test"), None, Language::Rust);
        let project = Project::Package(Box::new(package));
        let display = format!("{}", project);
        assert!(display.contains("unknown"));
    }
}
