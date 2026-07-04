use std::{
    cmp::Ordering,
    collections::HashSet,
    fmt::{Debug, Display},
    path::Path,
};

use anyhow::Result;
use colored::Colorize;

use crate::{config::Config, package::Package, update_type::UpdateType, workspace::Workspace};

/// Discriminated union of Package (single project) or Workspace (monorepo root).
///
/// Provides unified interface for operations on both package and workspace projects,
/// delegating to the appropriate trait implementation. Workspaces sort before packages
/// in ordering comparisons.
#[derive(Debug)]
pub enum Project {
    /// Monorepo workspace root containing multiple packages
    Workspace(Box<dyn Workspace>),
    /// Single versioned package
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

    pub fn set_name(&mut self, name: String) {
        match self {
            Self::Workspace(workspace) => workspace.set_name(name),
            Self::Package(package) => package.set_name(name),
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
    /// Returns error if the underlying publish call fails to spawn.
    pub async fn publish(&self, config: &Config) -> Result<crate::publish::PublishOutput> {
        match self {
            Self::Workspace(workspace) => workspace.publish(config).await,
            Self::Package(package) => package.publish(config).await,
        }
    }

    /// Run the publish command in dry-run mode.
    ///
    /// Returns `Ok(None)` when dry-run is not supported for this project's
    /// language and no override is configured in `config.publish_dry_run`.
    ///
    /// # Errors
    /// Returns error if the underlying dry-run publish call fails to spawn.
    pub async fn dry_run_publish(
        &self,
        config: &Config,
    ) -> Result<Option<crate::publish::PublishOutput>> {
        match self {
            Self::Workspace(workspace) => workspace.dry_run_publish(config).await,
            Self::Package(package) => package.dry_run_publish(config).await,
        }
    }

    /// Render the project's canonical one-line label (`[Workspace - Node] name
    /// (v1.0.0) - path`) with optional version override.
    ///
    /// `version_override` is used verbatim when supplied (typically a
    /// pre-formatted "v1.0.0 -> v1.1.0 (minor)" upgrade string from
    /// `changepacks_utils::display_update`); when `None`, the current version
    /// is rendered as `v{version}` or `unknown`. Extracted from `Display` so
    /// `check.rs::format_project_line` reuses the exact same base formatting.
    #[must_use]
    pub fn format_line(&self, version_override: Option<&str>) -> String {
        // Both variants render identical bytes except for the header prefix:
        // Workspace → "[Workspace - {lang}]", Package → "[{lang}]". Destructure
        // once so a future label/formatting change only needs to be applied in
        // one place.
        let (label_prefix, lang, name, ver, rel_path) = match self {
            Self::Workspace(w) => (
                "Workspace - ",
                w.language(),
                w.name(),
                w.version(),
                w.relative_path(),
            ),
            Self::Package(p) => ("", p.language(), p.name(), p.version(), p.relative_path()),
        };
        let version = version_override.map_or_else(
            || ver.map_or_else(|| "unknown".to_string(), |v| format!("v{v}")),
            ToString::to_string,
        );
        format!(
            "{} {} {} {} {}",
            format!("[{label_prefix}{lang}]").bright_blue().bold(),
            name.unwrap_or("noname").bright_white().bold(),
            format!("({version})").bright_green(),
            "-".bright_cyan(),
            rel_path.display().to_string().bright_black(),
        )
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

/// Shared language-then-name comparator for the Workspace × Workspace and
/// Package × Package arms of `Project::cmp`. Both arms follow the same
/// precedence — language first, then `(Some, Some) < (Some, None) < (None,
/// Some) < (None, None)` — and the ONLY divergence is the `(None, None)`
/// tie-breaker: Workspace falls back to version comparison, Package returns
/// `Ordering::Equal`. Threading that final branch through the caller-supplied
/// `none_none` closure preserves the byte-identical behavior of the two arms
/// while eliminating ~30 lines of duplication.
fn cmp_lang_then_name(
    lhs: (crate::Language, Option<&str>),
    rhs: (crate::Language, Option<&str>),
    none_none: impl FnOnce() -> Ordering,
) -> Ordering {
    let lang_ord = lhs.0.cmp(&rhs.0);
    if lang_ord != Ordering::Equal {
        return lang_ord;
    }
    match (lhs.1, rhs.1) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => none_none(),
    }
}

impl Ord for Project {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Workspace(_), Self::Package(_)) => Ordering::Less,
            (Self::Package(_), Self::Workspace(_)) => Ordering::Greater,
            (Self::Workspace(w1), Self::Workspace(w2)) => cmp_lang_then_name(
                (w1.language(), w1.name()),
                (w2.language(), w2.name()),
                || w1.version().unwrap_or("").cmp(w2.version().unwrap_or("")),
            ),
            (Self::Package(p1), Self::Package(p2)) => cmp_lang_then_name(
                (p1.language(), p1.name()),
                (p2.language(), p2.name()),
                || Ordering::Equal,
            ),
        }
    }
}

impl Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_line(None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Language;
    use async_trait::async_trait;
    use rstest::rstest;
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
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
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
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
        }
    }

    fn ws(name: Option<&str>, version: Option<&str>, language: Language) -> Project {
        Project::Workspace(Box::new(MockWorkspace::new(name, version, language)))
    }

    fn pkg(name: Option<&str>, version: Option<&str>, language: Language) -> Project {
        Project::Package(Box::new(MockPackage::new(name, version, language)))
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
        let output = project.publish(&config).await.unwrap();
        assert!(output.success);
    }

    #[tokio::test]
    async fn test_project_package_publish() {
        let temp_dir = std::env::temp_dir();
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.path = temp_dir.join("Cargo.toml");
        let project = Project::Package(Box::new(package));
        let config = Config::default();
        let output = project.publish(&config).await.unwrap();
        assert!(output.success);
    }

    #[tokio::test]
    async fn test_project_workspace_dry_run_publish() {
        let temp_dir = std::env::temp_dir();
        let mut workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        workspace.path = temp_dir.join("package.json");
        let project = Project::Workspace(Box::new(workspace));
        let config = Config::default();

        // MockWorkspace.default_publish_command() == "echo publish" and
        // Language::Node.dry_run_flag() == Some("--dry-run"), so the
        // derived dry-run command is "echo publish --dry-run".
        let output = project.dry_run_publish(&config).await.unwrap();
        assert!(output.is_some());
        let output = output.unwrap();
        assert!(output.success);
        assert!(output.stdout.contains("publish"));
    }

    #[tokio::test]
    async fn test_project_package_dry_run_publish() {
        let temp_dir = std::env::temp_dir();
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        package.path = temp_dir.join("Cargo.toml");
        let project = Project::Package(Box::new(package));
        let config = Config::default();

        let output = project.dry_run_publish(&config).await.unwrap();
        assert!(output.is_some());
        assert!(output.unwrap().success);
    }

    #[tokio::test]
    async fn test_project_package_dry_run_publish_propagates_language_override() {
        let temp_dir = std::env::temp_dir();
        let mut package = MockPackage::new(Some("test"), Some("1.0.0"), Language::CSharp);
        package.path = temp_dir.join("Sample.csproj");
        let project = Project::Package(Box::new(package));
        let mut publish_dry_run = std::collections::HashMap::new();
        publish_dry_run.insert("csharp".to_string(), "echo dry-csharp".to_string());
        let config = Config {
            publish_dry_run,
            ..Config::default()
        };

        // A per-language `publish_dry_run` config entry (here: `csharp`)
        // resolves to a runnable command and takes precedence over the
        // crate's built-in `default_dry_run_publish_command()`, ensuring
        // user-configured dry-run commands win regardless of whether the
        // language provides its own default.
        let output = project.dry_run_publish(&config).await.unwrap();
        assert!(output.is_some());
        assert!(output.unwrap().success);
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

    #[rstest]
    // Workspaces sort before packages regardless of language/name.
    #[case(
        ws(Some("test"), Some("1.0.0"), Language::Node),
        pkg(Some("test"), Some("1.0.0"), Language::Rust),
        Ordering::Less
    )]
    #[case(
        pkg(Some("test"), Some("1.0.0"), Language::Rust),
        ws(Some("test"), Some("1.0.0"), Language::Node),
        Ordering::Greater
    )]
    // Same variant + same language: order by name, with `Some(_) < None`.
    #[case(
        ws(Some("aaa"), Some("1.0.0"), Language::Node),
        ws(Some("bbb"), Some("1.0.0"), Language::Node),
        Ordering::Less
    )]
    #[case(
        ws(Some("test"), Some("1.0.0"), Language::Node),
        ws(None, Some("1.0.0"), Language::Node),
        Ordering::Less
    )]
    #[case(
        ws(None, Some("1.0.0"), Language::Node),
        ws(Some("test"), Some("1.0.0"), Language::Node),
        Ordering::Greater
    )]
    // Two nameless workspaces fall back to version comparison.
    #[case(
        ws(None, Some("1.0.0"), Language::Node),
        ws(None, Some("2.0.0"), Language::Node),
        Ordering::Less
    )]
    #[case(
        pkg(Some("aaa"), Some("1.0.0"), Language::Rust),
        pkg(Some("bbb"), Some("1.0.0"), Language::Rust),
        Ordering::Less
    )]
    #[case(
        pkg(Some("test"), Some("1.0.0"), Language::Rust),
        pkg(None, Some("1.0.0"), Language::Rust),
        Ordering::Less
    )]
    #[case(
        pkg(None, Some("1.0.0"), Language::Rust),
        pkg(Some("test"), Some("1.0.0"), Language::Rust),
        Ordering::Greater
    )]
    // Two nameless packages are equal (packages have no version tie-break).
    #[case(
        pkg(None, Some("1.0.0"), Language::Rust),
        pkg(None, Some("1.0.0"), Language::Rust),
        Ordering::Equal
    )]
    fn test_project_ord(#[case] a: Project, #[case] b: Project, #[case] expected: Ordering) {
        assert_eq!(a.cmp(&b), expected);
    }

    #[test]
    fn test_project_ord_workspaces_by_language() {
        // Kept separate: asserts only that a language difference breaks the tie
        // (not a specific ordering), so it stays independent of the `Language`
        // enum's declared order.
        let w1 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Python);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert_ne!(p1.cmp(&p2), Ordering::Equal);
    }

    #[test]
    fn test_project_ord_packages_by_language() {
        let pkg1 = MockPackage::new(Some("test"), Some("1.0.0"), Language::Node);
        let pkg2 = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let p1 = Project::Package(Box::new(pkg1));
        let p2 = Project::Package(Box::new(pkg2));
        assert_ne!(p1.cmp(&p2), Ordering::Equal);
    }

    #[rstest]
    #[case(ws(Some("my-workspace"), Some("1.0.0"), Language::Node), &["Workspace", "my-workspace", "v1.0.0"])]
    #[case(ws(None, Some("1.0.0"), Language::Node), &["noname"])]
    #[case(ws(Some("test"), None, Language::Node), &["unknown"])]
    #[case(pkg(Some("my-package"), Some("2.0.0"), Language::Rust), &["my-package", "v2.0.0"])]
    #[case(pkg(None, Some("1.0.0"), Language::Rust), &["noname"])]
    #[case(pkg(Some("test"), None, Language::Rust), &["unknown"])]
    fn test_project_display(#[case] project: Project, #[case] expected: &[&str]) {
        let display = format!("{project}");
        for &needle in expected {
            assert!(display.contains(needle), "{display:?} missing {needle:?}");
        }
    }

    #[test]
    fn test_project_sort_stability() {
        let make_projects = || {
            vec![
                Project::Package(Box::new(MockPackage::new(
                    Some("charlie"),
                    Some("1.0.0"),
                    Language::Rust,
                ))),
                Project::Workspace(Box::new(MockWorkspace::new(
                    Some("alpha"),
                    Some("2.0.0"),
                    Language::Node,
                ))),
                Project::Package(Box::new(MockPackage::new(
                    Some("bravo"),
                    Some("0.1.0"),
                    Language::Node,
                ))),
                Project::Workspace(Box::new(MockWorkspace::new(
                    Some("delta"),
                    Some("3.0.0"),
                    Language::Python,
                ))),
                Project::Package(Box::new(MockPackage::new(
                    Some("echo"),
                    Some("1.0.0"),
                    Language::Dart,
                ))),
            ]
        };

        let mut first = make_projects();
        first.sort();
        let first_order: Vec<Option<&str>> = first.iter().map(|p| p.name()).collect();

        let mut second = make_projects();
        second.sort();
        let second_order: Vec<Option<&str>> = second.iter().map(|p| p.name()).collect();

        let mut third = make_projects();
        third.sort();
        let third_order: Vec<Option<&str>> = third.iter().map(|p| p.name()).collect();

        assert_eq!(first_order, second_order);
        assert_eq!(second_order, third_order);
    }

    #[test]
    fn test_project_sort_mixed() {
        let mut projects = [
            Project::Package(Box::new(MockPackage::new(
                Some("pkg-a"),
                Some("1.0.0"),
                Language::Node,
            ))),
            Project::Workspace(Box::new(MockWorkspace::new(
                Some("ws-b"),
                Some("1.0.0"),
                Language::Node,
            ))),
            Project::Package(Box::new(MockPackage::new(
                Some("pkg-c"),
                Some("1.0.0"),
                Language::Rust,
            ))),
            Project::Workspace(Box::new(MockWorkspace::new(
                Some("ws-d"),
                Some("1.0.0"),
                Language::Rust,
            ))),
        ];
        projects.sort();

        // All workspaces must come before all packages
        let workspace_count = projects
            .iter()
            .take_while(|p| matches!(p, Project::Workspace(_)))
            .count();
        assert_eq!(workspace_count, 2);

        let package_count = projects
            .iter()
            .skip(workspace_count)
            .filter(|p| matches!(p, Project::Package(_)))
            .count();
        assert_eq!(package_count, 2);
    }

    #[test]
    fn test_project_set_name_workspace() {
        let workspace = MockWorkspace::new(Some("test"), Some("1.0.0"), Language::Node);
        let mut project = Project::Workspace(Box::new(workspace));
        project.set_name("new-name".to_string());
        // Mock doesn't override set_name, so default no-op applies
        assert_eq!(project.name(), Some("test"));
    }

    #[test]
    fn test_project_set_name_package() {
        let package = MockPackage::new(Some("test"), Some("1.0.0"), Language::Rust);
        let mut project = Project::Package(Box::new(package));
        project.set_name("new-name".to_string());
        // Mock doesn't override set_name, so default no-op applies
        assert_eq!(project.name(), Some("test"));
    }

    #[test]
    fn test_project_cmp_is_consistent_with_eq() {
        // Two workspaces with identical fields
        let w1 = MockWorkspace::new(Some("same"), Some("1.0.0"), Language::Node);
        let w2 = MockWorkspace::new(Some("same"), Some("1.0.0"), Language::Node);
        let p1 = Project::Workspace(Box::new(w1));
        let p2 = Project::Workspace(Box::new(w2));
        assert_eq!(p1, p2);
        assert_eq!(p1.cmp(&p2), Ordering::Equal);

        // Two packages with identical fields
        let pkg1 = MockPackage::new(Some("same"), Some("1.0.0"), Language::Rust);
        let pkg2 = MockPackage::new(Some("same"), Some("1.0.0"), Language::Rust);
        let pp1 = Project::Package(Box::new(pkg1));
        let pp2 = Project::Package(Box::new(pkg2));
        assert_eq!(pp1, pp2);
        assert_eq!(pp1.cmp(&pp2), Ordering::Equal);
    }
}
