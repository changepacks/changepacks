use core::{
    package::Package, proejct_finder::ProjectFinder, project::Project, workspace::Workspace,
};
use std::{
    collections::HashMap,
    fs::{canonicalize, read_to_string},
    path::Path,
};

use anyhow::{Context, Result};

pub struct RustProjectFinder {
    projects: HashMap<String, Project>,
}

impl RustProjectFinder {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
        }
    }
}

impl ProjectFinder for RustProjectFinder {
    fn projects(&self) -> Vec<&Project> {
        self.projects.values().collect::<Vec<_>>()
    }

    fn project_files(&self) -> &[&str] {
        &["Cargo.toml"]
    }

    fn visit(&mut self, path: &Path) -> Result<()> {
        if path.is_file()
            && self
                .project_files()
                .contains(&path.file_name().unwrap().to_str().unwrap())
        {
            let parent = path.parent().unwrap().to_string_lossy().to_string();
            if self.projects.contains_key(&parent) {
                return Ok(());
            }
            // read Cargo.toml
            let cargo_toml = read_to_string(path)?;
            let cargo_toml: toml::Value = toml::from_str(&cargo_toml)?;
            // if workspace
            if let Some(_) = cargo_toml.get("workspace") {
                let version = cargo_toml
                    .get("package")
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                self.projects.insert(
                    parent.clone(),
                    Project::Workspace(Workspace::new(parent, version)),
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
                    parent.clone(),
                    Project::Package(Package::new(name, version, parent)),
                );
            }
        }
        Ok(())
    }
}
