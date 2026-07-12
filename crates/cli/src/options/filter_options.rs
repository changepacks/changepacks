use changepacks_core::Project;
use clap::ValueEnum;

/// CLI filter for workspace-only or package-only listing.
///
/// Used by the check command to filter projects by type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FilterOptions {
    /// Show only workspace projects
    Workspace,
    /// Show only package projects
    Package,
}

impl FilterOptions {
    #[must_use]
    pub fn matches(self, project: &Project) -> bool {
        match self {
            Self::Workspace => matches!(project, Project::Workspace(_)),
            Self::Package => matches!(project, Project::Package(_)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use changepacks_core::Language;
    use clap::ValueEnum;
    use rstest::rstest;

    use changepacks_core::test_support::{MockPackage, MockWorkspace};

    fn workspace_project() -> Project {
        Project::Workspace(Box::new(MockWorkspace::with_all(
            Some("workspace"),
            Some("1.0.0"),
            "/repo/package.json",
            "package.json",
            Language::Node,
        )))
    }

    fn package_project() -> Project {
        Project::Package(Box::new(MockPackage::with_all(
            Some("package"),
            Some("1.0.0"),
            "/repo/crates/pkg/Cargo.toml",
            "crates/pkg/Cargo.toml",
            Language::Rust,
        )))
    }

    // `FilterOptions::X.matches(&project)` is true iff `project` is the
    // matching variant. Covers both `Workspace`/`Package` filters against
    // both project variants (4 = 2 filters × 2 project shapes).
    #[rstest]
    #[case(FilterOptions::Workspace, workspace_project(), true)]
    #[case(FilterOptions::Workspace, package_project(), false)]
    #[case(FilterOptions::Package, package_project(), true)]
    #[case(FilterOptions::Package, workspace_project(), false)]
    fn test_filter_options_matches(
        #[case] filter: FilterOptions,
        #[case] project: Project,
        #[case] expected: bool,
    ) {
        assert_eq!(filter.matches(&project), expected);
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
