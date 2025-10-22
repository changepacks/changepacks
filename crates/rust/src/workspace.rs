use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};
use utils::next_version;

#[derive(Debug)]
pub struct RustWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
}

impl RustWorkspace {
    pub fn new(
        name: Option<String>,
        version: Option<String>,
        path: PathBuf,
        relative_path: PathBuf,
    ) -> Self {
        Self {
            path,
            relative_path,
            name,
            version,
            is_changed: false,
        }
    }
}

#[async_trait]
impl Workspace for RustWorkspace {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    async fn update_version(&self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(
            self.version.as_ref().unwrap_or(&String::from("0.0.0")),
            update_type,
        )?;

        let cargo_toml = read_to_string(&self.path).await?;
        let mut cargo_toml: toml::Value = toml::from_str(&cargo_toml)?;
        cargo_toml["package"]["version"] = toml::Value::String(next_version);
        write(&self.path, toml::to_string_pretty(&cargo_toml)?).await?;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }

    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }

    fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}
