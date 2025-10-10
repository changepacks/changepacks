use core::{package::Package, proejct_finder::ProjectFinder, project::Project};
use std::{collections::HashMap, fs::canonicalize, path::Path};

use anyhow::Result;

pub struct PythonProjectFinder {
    projects: HashMap<String, Project>,
}

impl PythonProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
        }
    }
}

impl ProjectFinder for PythonProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &["pyproject.toml"]
    }

    fn visit(&mut self, path: &Path) -> Result<()> {
        if path.is_file()
            && self
                .project_files()
                .contains(&path.file_name().unwrap().to_str().unwrap())
        {
            let parent = canonicalize(path.parent().unwrap())
                .unwrap()
                .to_string_lossy()
                .to_string();
            if self.projects.contains_key(&parent) {
                return Ok(());
            }
            self.projects.insert(
                parent.clone(),
                Project::Package(Package::new(
                    "python".to_string(),
                    "1.0.0".to_string(),
                    parent,
                )),
            );
        }
        Ok(())
    }
}
