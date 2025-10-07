use crate::{package::Package, workspace::Workspace};

pub enum Project {
    Workspace(Workspace),
    Package(Package),
}

impl Project {
    pub fn get_packages(&self) -> Vec<&Package> {
        match self {
            Project::Workspace(workspace) => workspace.packages.iter().collect(),
            Project::Package(package) => vec![package],
        }
    }
}
