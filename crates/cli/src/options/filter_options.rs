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
