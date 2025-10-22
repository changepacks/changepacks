use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};
use utils::next_version;

#[derive(Debug)]
pub struct RustPackage {
    name: String,
    version: String,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
}

impl RustPackage {
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
impl Package for RustPackage {
    fn relative_path(&self) -> &Path {
        &self.relative_path
    }
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn path(&self) -> &Path {
        &self.path
    }

    async fn update_version(&self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(&self.version, update_type)?;

        let cargo_toml = read_to_string(&self.path).await?;
        let mut cargo_toml: toml::Value = toml::from_str(&cargo_toml)?;
        cargo_toml["package"]["version"] = toml::Value::String(next_version);
        write(&self.path, toml::to_string_pretty(&cargo_toml)?).await?;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }
}
