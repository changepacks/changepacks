use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use tokio::fs::{read_to_string, write};
use utils::next_version;

#[derive(Debug)]
pub struct DartPackage {
    name: String,
    version: String,
    path: PathBuf,
    relative_path: PathBuf,
    is_changed: bool,
}

impl DartPackage {
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
impl Package for DartPackage {
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

        let pubspec_yaml = read_to_string(&self.path).await?;
        let mut pubspec: serde_yaml::Value = serde_yaml::from_str(&pubspec_yaml)?;
        pubspec["version"] = serde_yaml::Value::String(next_version);
        write(&self.path, serde_yaml::to_string(&pubspec)?).await?;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Dart
    }

    fn is_changed(&self) -> bool {
        self.is_changed
    }
    fn set_changed(&mut self, changed: bool) {
        self.is_changed = changed;
    }
}
