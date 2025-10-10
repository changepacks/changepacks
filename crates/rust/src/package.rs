use anyhow::Result;
use core::{Package, update_type::UpdateType};

#[derive(Debug)]
pub struct RustPackage {
    name: String,
    version: String,
    path: String,
}

impl RustPackage {
    pub fn new(name: String, version: String, path: String) -> Self {
        Self {
            name,
            version,
            path,
        }
    }
}

impl Package for RustPackage {
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
        todo!("Rust package version update logic")
    }

    fn language(&self) -> &str {
        "Rust"
    }
}
