use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::next_version;
use tokio::fs::{read_to_string, write};

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
        write(
            &self.path,
            yamlpatch::apply_yaml_patches(
                &yamlpath::Document::new(&pubspec_yaml).context("Failed to parse YAML")?,
                &[yamlpatch::Patch {
                    operation: yamlpatch::Op::Replace(serde_yaml::Value::String(next_version)),
                    route: yamlpath::route!("version"),
                }],
            )?
            .source(),
        )
        .await?;
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
