use anyhow::{Context, Result};
use async_trait::async_trait;
use changepack_core::ProjectFinder;
use changepack_core::project::Project;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use tokio::fs::read_to_string;

use crate::{package::DartPackage, workspace::DartWorkspace};

#[derive(Debug)]
pub struct DartProjectFinder {
    projects: HashMap<PathBuf, Project>,
    project_files: Vec<&'static str>,
}

impl Default for DartProjectFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl DartProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            project_files: vec!["pubspec.yaml"],
        }
    }
}

#[async_trait]
impl ProjectFinder for DartProjectFinder {
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
        // glob all the pubspec.yaml in the root without .gitignore
        if path.is_file()
            && self.project_files().contains(
                &path
                    .file_name()
                    .context("File name not found")?
                    .to_str()
                    .context("File name not found")?,
            )
        {
            if self.projects.contains_key(path) {
                return Ok(());
            }
            // read pubspec.yaml
            let pubspec_yaml = read_to_string(path).await?;
            let pubspec: serde_yaml::Value = serde_yaml::from_str(&pubspec_yaml)?;

            // Check if this is a workspace (melos workspace or similar)
            let is_workspace = pubspec.get("workspace").is_some()
                || path
                    .parent()
                    .context("Parent not found")?
                    .join("melos.yaml")
                    .is_file();

            if is_workspace {
                let version = pubspec["version"].as_str().map(|v| v.to_string());
                let name = pubspec["name"].as_str().map(|v| v.to_string());
                self.projects.insert(
                    path.to_path_buf(),
                    Project::Workspace(Box::new(DartWorkspace::new(
                        name,
                        version,
                        path.to_path_buf(),
                        relative_path.to_path_buf(),
                    ))),
                );
            } else {
                let version = pubspec["version"]
                    .as_str()
                    .context("Version not found")?
                    .to_string();
                let name = pubspec["name"]
                    .as_str()
                    .context("Name not found")?
                    .to_string();

                self.projects.insert(
                    path.to_path_buf(),
                    Project::Package(Box::new(DartPackage::new(
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
