use crate::{package::Package, workspace::Workspace};

#[derive(Debug)]
pub enum Project {
    Workspace(Workspace),
    Package(Package),
}

impl Project {
    pub fn get_packages(&self) -> Vec<&Package> {
        match self {
            Project::Workspace(_) => vec![],
            Project::Package(package) => vec![package],
        }
    }
}
