use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Project, ProjectFinder};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::NodePackage, workspace::NodeWorkspace};

#[derive(Debug)]
pub struct NodeProjectFinder {
    projects: HashMap<PathBuf, Project>,
    project_files: Vec<&'static str>,
}

impl Default for NodeProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec!["package.json"],
        }
    }
}

#[async_trait]
impl ProjectFinder for NodeProjectFinder {
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
        // glob all the package.json in the root without .gitignore
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
            // read package.json
            let package_json = read_to_string(path).await?;
            let package_json: serde_json::Value = serde_json::from_str(&package_json)?;
            // if workspaces
            if package_json.get("workspaces").is_some()
                || path
                    .parent()
                    .context(format!("Parent not found - {}", path.display()))?
                    .join("pnpm-workspace.yaml")
                    .is_file()
            {
                let version = package_json["version"].as_str().map(|v| v.to_string());
                let name = package_json["name"].as_str().map(|v| v.to_string());
                self.projects.insert(
                    path.to_path_buf(),
                    Project::Workspace(Box::new(NodeWorkspace::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                );
            } else {
                let version = package_json["version"]
                    .as_str()
                    .context(format!("Version not found - {}", path.display()))?
                    .to_string();
                let name = package_json["name"]
                    .as_str()
                    .context(format!("Name not found - {}", path.display()))?
                    .to_string();

                self.projects.insert(
                    path.to_path_buf(),
                    Project::Package(Box::new(NodePackage::new(
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
