use anyhow::{Context, Result};
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use changepacks_utils::next_version;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};

#[derive(Debug)]
pub struct DartWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
}

impl DartWorkspace {
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
impl Workspace for DartWorkspace {
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
    fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}
