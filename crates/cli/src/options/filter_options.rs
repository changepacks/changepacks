use changepacks_core::Project;
use clap::ValueEnum;

use super::language_options::{CliLanguage, retain_by_language};

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

/// Apply the shared `--filter` + `--language` retention pass to `projects`.
///
/// The two commands that expose both flags — `check` and the default
/// `changepack` flow — previously open-coded the identical pair of statements:
/// an `args.filter` retain guarded by `if let Some(..)`, immediately followed by
/// [`retain_by_language`]. The language half was already extracted; this is the
/// missing `FilterOptions` half, so the combined selection rule now lives in one
/// place.
///
/// Behavior is byte-identical to the inlined version: `filter` is applied first
/// and only when present, then the language filter runs (itself a no-op for an
/// empty `langs`). `Vec::retain` is order-stable and both predicates still run
/// in the same sequence, so the surviving projects and their relative order are
/// unchanged.
///
/// `publish` deliberately does not use this helper — it has no `--filter` flag
/// and correctly applies only [`retain_by_language`].
///
/// `filter` is taken by value: [`FilterOptions`] is a fieldless two-variant
/// `Copy` enum, so a `&FilterOptions` is strictly larger than the value it
/// points at and forces every caller to write `.as_ref()` for nothing.
pub fn retain_by_filters(
    projects: &mut Vec<&Project>,
    filter: Option<FilterOptions>,
    langs: &[CliLanguage],
) {
    if let Some(filter) = filter {
        projects.retain(|project| filter.matches(project));
    }
    retain_by_language(langs, projects);
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

    fn rust_package() -> Project {
        Project::Package(Box::new(MockPackage::with_all(
            Some("rust-pkg"),
            Some("1.0.0"),
            "/repo/crates/pkg/Cargo.toml",
            "crates/pkg/Cargo.toml",
            Language::Rust,
        )))
    }

    fn node_package() -> Project {
        Project::Package(Box::new(MockPackage::with_all(
            Some("node-pkg"),
            Some("1.0.0"),
            "/repo/packages/pkg/package.json",
            "packages/pkg/package.json",
            Language::Node,
        )))
    }

    /// Locks the combined `--filter` + `--language` retention rule that `check`
    /// and `changepack` share. Fixture order is workspace(Node),
    /// package(Rust), package(Node), so each case also proves `Vec::retain`
    /// keeps the survivors in their original relative order.
    ///
    /// Expected values are the repo-relative paths of the surviving projects.
    #[rstest]
    // No flags at all: pure no-op, everything survives in order.
    #[case(None, &[], &["package.json", "crates/pkg/Cargo.toml", "packages/pkg/package.json"])]
    // `--filter` only.
    #[case(Some(FilterOptions::Workspace), &[], &["package.json"])]
    #[case(Some(FilterOptions::Package), &[], &["crates/pkg/Cargo.toml", "packages/pkg/package.json"])]
    // `--language` only.
    #[case(None, &[CliLanguage::Node], &["package.json", "packages/pkg/package.json"])]
    // Both flags: the filter runs first, then the language filter.
    #[case(Some(FilterOptions::Package), &[CliLanguage::Node], &["packages/pkg/package.json"])]
    #[case(Some(FilterOptions::Workspace), &[CliLanguage::Rust], &[])]
    #[case(Some(FilterOptions::Package), &[CliLanguage::Node, CliLanguage::Rust], &["crates/pkg/Cargo.toml", "packages/pkg/package.json"])]
    fn test_retain_by_filters(
        #[case] filter: Option<FilterOptions>,
        #[case] langs: &[CliLanguage],
        #[case] expected: &[&str],
    ) {
        let projects = [workspace_project(), rust_package(), node_package()];
        let mut refs: Vec<&Project> = projects.iter().collect();

        retain_by_filters(&mut refs, filter, langs);

        let actual: Vec<String> = refs
            .iter()
            .map(|p| p.relative_path().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(actual, expected);
    }
}
