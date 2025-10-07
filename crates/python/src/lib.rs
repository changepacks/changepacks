use core::{proejct_finder::ProjectFinder, project::Project};

use utils::find_root_path_by_files;

pub struct PythonProjectFinder {
    root: String,
}

impl ProjectFinder for PythonProjectFinder {
    fn new(root: Option<String>) -> Self {
        if let Some(root) = root {
            Self { root }
        } else {
            Self {
                root: find_root_path_by_files(".", &["pyproject.toml"]).unwrap_or(".".to_string()),
            }
        }
    }

    fn find(&self) -> Vec<Project> {
        vec![]
    }
}
