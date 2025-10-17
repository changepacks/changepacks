use anyhow::{Context, Result};
use async_trait::async_trait;
use changepack_core::{ProjectFinder, project::Project};
use std::{collections::HashMap, path::Path};
use tokio::fs::read_to_string;

use crate::{package::RustPackage, workspace::RustWorkspace};

#[derive(Debug)]
pub struct RustProjectFinder {
    projects: HashMap<String, Project>,
    project_files: Vec<&'static str>,
}

impl Default for RustProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl RustProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec!["Cargo.toml"],
        }
    }
}

#[async_trait]
impl ProjectFinder for RustProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &self.project_files
    }

    async fn visit(&mut self, path: &Path) -> Result<()> {
        if path.is_file()
            && self
                .project_files()
                .contains(&path.file_name().unwrap().to_str().unwrap())
        {
            let file_path = path.to_string_lossy().to_string();
            if self.projects.contains_key(&file_path) {
                return Ok(());
            }
            // read Cargo.toml
            let cargo_toml = read_to_string(path).await?;
            let cargo_toml: toml::Value = toml::from_str(&cargo_toml)?;
            // if workspace
            if cargo_toml.get("workspace").is_some() {
                let version = cargo_toml
                    .get("package")
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let name = cargo_toml
                    .get("package")
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                self.projects.insert(
                    file_path.clone(),
                    Project::Workspace(Box::new(RustWorkspace::new(file_path, name, version))),
                );
            } else {
                let version = cargo_toml["package"]["version"]
                    .as_str()
                    .context("Version not found")?
                    .to_string();
                let name = cargo_toml["package"]["name"]
                    .as_str()
                    .context("Name not found")?
                    .to_string();
                self.projects.insert(
                    file_path.clone(),
                    Project::Package(Box::new(RustPackage::new(name, version, file_path))),
                );
            }
        }
        Ok(())
    }
}
