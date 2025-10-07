use core::{proejct_finder::ProjectFinder, project::Project};

use utils::find_root_path_by_files;

pub struct RustProjectFinder {
    root: String,
}

impl ProjectFinder for RustProjectFinder {
    fn new(root: Option<String>) -> Self {
        if let Some(root) = root {
            Self { root }
        } else {
            Self {
                root: find_root_path_by_files(".", &["Cargo.toml"]).unwrap_or(".".to_string()),
            }
        }
    }

    fn find(&self) -> Vec<Project> {
        vec![]
    }
}
