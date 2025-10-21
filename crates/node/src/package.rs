use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, update_type::UpdateType};
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};
use utils::next_version;

#[derive(Debug)]
pub struct NodePackage {
    name: String,
    version: String,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
}

impl NodePackage {
    pub fn new(name: String, version: String, path: PathBuf, relative_path: PathBuf) -> Self {
        Self {
            name,
            version,
            path,
            relative_path,
            is_changed: false,
        }
    }
}

#[async_trait]
impl Package for NodePackage {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    async fn update_version(&self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(&self.version, update_type)?;

        let package_json = read_to_string(&self.path).await?;
        let mut package_json: serde_json::Value = serde_json::from_str(&package_json)?;
        package_json["version"] = serde_json::Value::String(next_version);
        write(&self.path, serde_json::to_string_pretty(&package_json)?).await?;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Node
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }
    fn is_changed(&self) -> bool {
        self.is_changed
    }
}
