use anyhow::Result;
use async_trait::async_trait;
use changepack_core::{Workspace, update_type::UpdateType};
use tokio::fs::{read_to_string, write};
use utils::next_version;

#[derive(Debug)]
pub struct RustWorkspace {
    path: String,
    version: Option<String>,
    name: Option<String>,
}

impl RustWorkspace {
    pub fn new(path: String, name: Option<String>, version: Option<String>) -> Self {
        Self {
            path,
            name,
            version,
        }
    }
}

#[async_trait]
impl Workspace for RustWorkspace {
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
            &self.version.as_ref().unwrap_or(&String::from("0.0.0")),
            update_type,
        )?;

        let cargo_toml = read_to_string(&self.path).await?;
        let mut cargo_toml: toml::Value = toml::from_str(&cargo_toml)?;
        cargo_toml["package"]["version"] = toml::Value::String(next_version);
        write(&self.path, toml::to_string_pretty(&cargo_toml)?).await?;
        Ok(())
    }

    fn language(&self) -> &str {
        "Rust"
    }
}
