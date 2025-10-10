use anyhow::Result;
use core::{Workspace, update_type::UpdateType};

#[derive(Debug)]
pub struct NodeWorkspace {
    path: String,
    version: Option<String>,
    name: Option<String>,
}

impl NodeWorkspace {
    pub fn new(path: String, name: Option<String>, version: Option<String>) -> Self {
        Self {
            path,
            name,
            version,
        }
    }
}

impl Workspace for NodeWorkspace {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    fn update_version(&mut self, update_type: UpdateType) -> Result<String> {
        todo!("Node.js workspace version update logic")
    }

    fn language(&self) -> &str {
        "Node.js"
    }
}
