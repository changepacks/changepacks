use core::{proejct_finder::ProjectFinder, project::Project};
use std::{
    fs,
    path::{Path, PathBuf},
};

use utils::find_root_path_by_files;

pub struct NodeProjectFinder {
    root: String,
}

impl ProjectFinder for NodeProjectFinder {
    fn new(root: Option<String>) -> Self {
        if let Some(root) = root {
            Self { root }
        } else {
            Self {
                root: find_root_path_by_files(".", &["package.json"]).unwrap_or(".".to_string()),
            }
        }
    }

    fn find(&self) -> Vec<Project> {
        // glob all the package.json in the root without .gitignore
        vec![]
    }
}
