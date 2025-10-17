use anyhow::{Context, Result};
use async_trait::async_trait;
use changepack_core::ProjectFinder;
use changepack_core::project::Project;
use std::{collections::HashMap, path::Path};
use tokio::fs::read_to_string;

use crate::{package::NodePackage, workspace::NodeWorkspace};

#[derive(Debug)]
pub struct NodeProjectFinder {
    projects: HashMap<String, Project>,
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

    fn project_files(&self) -> &[&str] {
        &self.project_files
    }

    async fn visit(&mut self, path: &Path) -> Result<()> {
        // glob all the package.json in the root without .gitignore
        if path.is_file()
            && self
                .project_files()
                .contains(&path.file_name().unwrap().to_str().unwrap())
        {
            let file_path = path.to_string_lossy().to_string();
            if self.projects.contains_key(&file_path) {
                return Ok(());
            }
            // read package.json
            let package_json = read_to_string(path).await?;
            let package_json: serde_json::Value = serde_json::from_str(&package_json)?;
            // if workspaces
            if package_json.get("workspaces").is_some()
                || path.parent().unwrap().join("pnpm-workspace.yaml").is_file()
            {
                let version = package_json["version"].as_str().map(|v| v.to_string());
                let name = package_json["name"].as_str().map(|v| v.to_string());
                self.projects.insert(
                    file_path.clone(),
                    Project::Workspace(Box::new(NodeWorkspace::new(file_path, name, version))),
                );
            } else {
                let version = package_json["version"]
                    .as_str()
                    .context("Version not found")?
                    .to_string();
                let name = package_json["name"]
                    .as_str()
                    .context("Name not found")?
                    .to_string();

                self.projects.insert(
                    file_path.clone(),
                    Project::Package(Box::new(NodePackage::new(name, version, file_path))),
                );
            }
        }
        Ok(())
    }
}
