use anyhow::Result;
use async_trait::async_trait;
use changepack_core::{Language, Package, update_type::UpdateType};
use tokio::fs::{read_to_string, write};
use utils::next_version;

#[derive(Debug)]
pub struct PythonPackage {
    name: String,
    version: String,
    path: String,
}

impl PythonPackage {
    pub fn new(name: String, version: String, path: String) -> Self {
        Self {
            name,
            version,
            path,
        }
    }
}

#[async_trait]
impl Package for PythonPackage {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn path(&self) -> &str {
        &self.path
    }

    async fn update_version(&self, update_type: UpdateType) -> Result<()> {
        let next_version = next_version(&self.version, update_type)?;

        let pyproject_toml = read_to_string(&self.path).await?;
        let mut pyproject_toml: toml::Value = toml::from_str(&pyproject_toml)?;
        pyproject_toml["project"]["version"] = toml::Value::String(next_version);
        write(&self.path, toml::to_string_pretty(&pyproject_toml)?).await?;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Python
    }
}
