use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{ProjectFinder, project::Project};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::PythonPackage, workspace::PythonWorkspace};

#[derive(Debug)]
pub struct PythonProjectFinder {
    projects: HashMap<PathBuf, Project>,
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

#[async_trait]
impl ProjectFinder for PythonProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }
    fn projects_mut(&mut self) -> Vec<&mut Project> {
        self.projects.values_mut().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &self.project_files
    }

    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()> {
        if path.is_file()
            && self.project_files().contains(
                &path
                    .file_name()
                    .context(format!("File name not found - {}", path.display()))?
                    .to_str()
                    .context(format!("File name not found - {}", path.display()))?,
            )
        {
            if self.projects.contains_key(path) {
                return Ok(());
            }
            // read pyproject.toml
            let pyproject_toml = read_to_string(path).await?;
            let pyproject_toml: toml::Value = toml::from_str(&pyproject_toml)?;
            let project = pyproject_toml
                .get("project")
                .context(format!("Project not found - {}", path.display()))?;

            // if workspace
            if pyproject_toml
                .get("tool")
                .and_then(|t| t.get("uv").and_then(|u| u.get("workspace")))
                .is_some()
            {
                let version = project["version"].as_str().map(|v| v.to_string());
                let name = project["name"].as_str().map(|v| v.to_string());
                self.projects.insert(
                    path.to_path_buf(),
                    Project::Workspace(Box::new(PythonWorkspace::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                );
            } else {
                let version = project
                    .get("version")
                    .context(format!("Version not found - {}", path.display()))?
                    .to_string();
                let name = project
                    .get("name")
                    .context(format!("Name not found - {}", path.display()))?
                    .to_string();
                self.projects.insert(
                    path.to_path_buf(),
                    Project::Package(Box::new(PythonPackage::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                );
            }
        }
        Ok(())
    }
}
