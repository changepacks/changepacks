use changepacks_core::Project;
use clap::ValueEnum;

#[derive(Debug, Clone, ValueEnum)]
pub enum FilterOptions {
    Workspace,
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
