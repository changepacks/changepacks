use anyhow::Result;
use core::{Package, update_type::UpdateType};

#[derive(Debug)]
pub struct NodePackage {
    name: String,
    version: String,
    path: String,
}

impl NodePackage {
    pub fn new(name: String, version: String, path: String) -> Self {
        Self {
            name,
            version,
            path,
        }
    }
}

impl Package for NodePackage {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn update_version(&mut self, update_type: UpdateType) -> Result<String> {
        todo!("Node.js package version update logic")
    }

    fn language(&self) -> &str {
        "Node.js"
    }
}
