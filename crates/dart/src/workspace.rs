use anyhow::Result;
use async_trait::async_trait;
use changepack_core::{update_type::UpdateType, Language, Workspace};
use std::path::Path;
use tokio::fs::{read_to_string, write};
use utils::next_version;

#[derive(Debug)]
pub struct DartWorkspace {
    path: String,
    version: Option<String>,
    name: Option<String>,
}

impl DartWorkspace {
    pub fn new(path: String, name: Option<String>, version: Option<String>) -> Self {
        Self {
            path,
            name,
            version,
        }
    }
}

#[async_trait]
impl Workspace for DartWorkspace {
    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn path(&self) -> &str {
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

        let pubspec_yaml = read_to_string(Path::new(&self.path)).await?;
        let mut pubspec: serde_yaml::Value = serde_yaml::from_str(&pubspec_yaml)?;
        pubspec["version"] = serde_yaml::Value::String(next_version.clone());
        write(Path::new(&self.path), serde_yaml::to_string(&pubspec)?).await?;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Dart
    }
}
