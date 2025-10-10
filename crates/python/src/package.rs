use anyhow::Result;
use core::{Package, update_type::UpdateType};

#[derive(Debug)]
pub struct PythonPackage {
    name: String,
    version: String,
    path: String,
}

impl PythonPackage {
    pub fn new(name: String, version: String, path: String) -> Self {
        Self {
            name,
            version,
            path,
        }
    }
}

impl Package for PythonPackage {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn update_version(&self, update_type: UpdateType) -> Result<()> {
        todo!("Python package version update logic")
    }

    fn language(&self) -> &str {
        "Python"
    }
}
