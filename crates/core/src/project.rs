use std::{cmp::Ordering, fmt::Debug};

use anyhow::Result;

use crate::{package::Package, update_type::UpdateType, workspace::Workspace};

#[derive(Debug)]
pub enum Project {
    Workspace(Box<dyn Workspace>),
    Package(Box<dyn Package>),
}

impl Project {
    pub fn name(&self) -> Option<&str> {
        match self {
            Project::Workspace(workspace) => workspace.name(),
            Project::Package(package) => Some(package.name()),
        }
    }

    pub fn version(&self) -> Option<&str> {
        match self {
            Project::Workspace(workspace) => workspace.version(),
            Project::Package(package) => Some(package.version()),
        }
    }
    pub fn path(&self) -> &str {
        match self {
            Project::Workspace(workspace) => workspace.path(),
            Project::Package(package) => package.path(),
        }
    }

    pub async fn update_version(&self, update_type: UpdateType) -> Result<()> {
        match self {
            Project::Workspace(workspace) => workspace.update_version(update_type.clone()).await?,
            Project::Package(package) => package.update_version(update_type.clone()).await?,
        }
        Ok(())
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
            (Project::Workspace(_), Project::Package(_)) => Ordering::Less,
            (Project::Package(_), Project::Workspace(_)) => Ordering::Greater,
            (Project::Workspace(w1), Project::Workspace(w2)) => {
                let lang_ord = w1.language().cmp(w2.language());
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
            (Project::Package(p1), Project::Package(p2)) => {
                let lang_ord = p1.language().cmp(p2.language());
                if lang_ord != Ordering::Equal {
                    return lang_ord;
                }
                p1.name().cmp(p2.name())
            }
        }
    }
}
