use core::{package::Package, proejct_finder::ProjectFinder, project::Project};
use std::{
    collections::HashMap,
    fs::{canonicalize, read_to_string},
    path::Path,
};

use anyhow::{Context, Result};

pub struct NodeProjectFinder {
    projects: HashMap<String, Project>,
}

impl NodeProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
        }
    }
}

impl ProjectFinder for NodeProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &["package.json"]
    }

    fn visit(&mut self, path: &Path) -> Result<()> {
        // glob all the package.json in the root without .gitignore
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
            // read package.json
            let package_json = read_to_string(path)?;
            let package_json: serde_json::Value = serde_json::from_str(&package_json)?;
            let version = package_json["version"]
                .as_str()
                .context("Version not found")?
                .to_string();
            let name = package_json["name"]
                .as_str()
                .context("Name not found")?
                .to_string();

            self.projects.insert(
                parent.clone(),
                Project::Package(Package::new(name, version, parent)),
            );
        }
        Ok(())
    }
}
