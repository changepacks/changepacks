use anyhow::{Context, Result};
use core::{ProjectFinder, project::Project};
use std::{collections::HashMap, fs::read_to_string, path::Path};

use crate::{package::PythonPackage, workspace::PythonWorkspace};

pub struct PythonProjectFinder {
    projects: HashMap<String, Project>,
    project_files: Vec<&'static str>,
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
            project_files: vec!["pyproject.toml"],
        }
    }
}

impl ProjectFinder for PythonProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &self.project_files
    }

    fn visit(&mut self, path: &Path) -> Result<()> {
        if path.is_file()
            && self
                .project_files()
                .contains(&path.file_name().unwrap().to_str().unwrap())
        {
            let file_path = path.to_string_lossy().to_string();
            if self.projects.contains_key(&file_path) {
                return Ok(());
            }
            // read pyproject.toml
            let pyproject_toml = read_to_string(path)?;
            let pyproject_toml: toml::Value = toml::from_str(&pyproject_toml)?;
            // if workspace
            if pyproject_toml
                .get("tool")
                .and_then(|t| t.get("uv").and_then(|u| u.get("workspace")))
                .is_some()
            {
                let version = pyproject_toml["project"]["version"]
                    .as_str()
                    .map(|v| v.to_string());
                let name = pyproject_toml["project"]["name"]
                    .as_str()
                    .map(|v| v.to_string());
                self.projects.insert(
                    file_path.clone(),
                    Project::Workspace(Box::new(PythonWorkspace::new(file_path, name, version))),
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
                    file_path.clone(),
                    Project::Package(Box::new(PythonPackage::new(name, version, file_path))),
                );
            }
        }
        Ok(())
    }
}
