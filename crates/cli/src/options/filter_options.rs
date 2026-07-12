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
    use rstest::rstest;
    use std::collections::HashSet;
    use std::path::PathBuf;

    // Field name `is_changed` matches the `impl_basic_accessors!()` macro
    // contract (see `crates/core/src/project_finder.rs`). Adopting the shared
    // macros here locks the field-name contract at one more test surface —
    // the same way every `Package`/`Workspace` mock in
    // `crates/core/src/{package,workspace,project,project_finder}.rs::tests`
    // and `crates/cli/src/commands/check.rs::tests` already does.
    #[derive(Debug)]
    struct MockPackage {
        name: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        version: Option<String>,
        language: Language,
        dependencies: HashSet<String>,
        is_changed: bool,
    }

    #[async_trait]
    impl Package for MockPackage {
        // Same macro adoption as every real-world `Package` impl and every
        // other test mock in the workspace — collapses the seven trivial
        // accessors (`name`, `version`, `path`, `relative_path`,
        // `is_changed`, `set_changed`, `set_name`) into one invocation and
        // the two dependency accessors (`dependencies`, `add_dependency`)
        // into another.
        changepacks_core::impl_basic_accessors!();
        changepacks_core::impl_dependencies_accessors!();

        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }

        fn language(&self) -> Language {
            self.language
        }

        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
        }
    }

    // Field name `is_changed` matches the `impl_basic_accessors!()` macro
    // contract (see `MockPackage` above for rationale).
    #[derive(Debug)]
    struct MockWorkspace {
        name: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
        version: Option<String>,
        language: Language,
        dependencies: HashSet<String>,
        is_changed: bool,
    }

    #[async_trait]
    impl Workspace for MockWorkspace {
        // Same macro adoption as `MockPackage` above — the `Workspace`
        // trait carries the same trivial accessors as `Package`, so one
        // `impl_basic_accessors!()` + `impl_dependencies_accessors!()` pair
        // covers both trait shapes.
        changepacks_core::impl_basic_accessors!();
        changepacks_core::impl_dependencies_accessors!();

        async fn update_version(&mut self, _update_type: UpdateType) -> anyhow::Result<()> {
            Ok(())
        }

        fn language(&self) -> Language {
            self.language
        }

        fn default_publish_command(&self) -> String {
            "echo publish".to_string()
        }
        fn default_dry_run_publish_command(&self) -> Option<String> {
            Some("echo publish --dry-run".to_string())
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
            is_changed: false,
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
            is_changed: false,
        }))
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
