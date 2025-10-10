use core::{
    package::Package, proejct_finder::ProjectFinder, project::Project, workspace::Workspace,
};
use std::{collections::HashMap, fs::read_to_string, path::Path};

use anyhow::{Context, Result};

pub struct PythonProjectFinder {
    projects: HashMap<String, Project>,
}

impl Default for PythonProjectFinder {
    fn default() -> Self {
        Self::new()
    }
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
            let parent = path.parent().unwrap();
            let parent_str = parent.to_string_lossy().to_string();
            if self.projects.contains_key(&parent_str) {
                return Ok(());
            }
            // read pyproject.toml
            let pyproject_toml = read_to_string(path)?;
            let pyproject_toml: toml::Value = toml::from_str(&pyproject_toml)?;
            // if workspace
            if pyproject_toml
                .get("tool")
                .and_then(|t| t.get("uv").and_then(|u| u.get("workspace"))).is_some()
            {
                let version = pyproject_toml["project"]["version"]
                    .as_str()
                    .map(|v| v.to_string());
                self.projects.insert(
                    parent_str.clone(),
                    Project::Workspace(Workspace::new(parent_str, version)),
                );
            } else {
                let version = pyproject_toml["project"]["version"]
                    .as_str()
                    .context("Version not found")?
                    .to_string();
                let name = pyproject_toml["project"]["name"]
                    .as_str()
                    .context("Name not found")?
                    .to_string();
                self.projects.insert(
                    parent_str.clone(),
                    Project::Package(Package::new(name, version, parent_str)),
                );
            }
        }
        Ok(())
    }
}
