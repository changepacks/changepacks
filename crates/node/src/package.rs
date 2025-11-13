use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, Package, UpdateType};
use changepacks_utils::{detect_indent, next_version};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};

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

        let package_json_raw = read_to_string(&self.path).await?;
        let postfix = if package_json_raw.ends_with("\n") {
            "\n"
        } else {
            ""
        };
        let indent = detect_indent(&package_json_raw);
        let mut package_json: serde_json::Value = serde_json::from_str(&package_json_raw)?;
        package_json["version"] = serde_json::Value::String(next_version);
        let ind = &b" ".repeat(indent);
        let formatter = serde_json::ser::PrettyFormatter::with_indent(ind);
        let writer = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(writer, formatter);
        package_json.serialize(&mut ser)?;
        write(
            &self.path,
            String::from_utf8(ser.into_inner())?.to_string() + postfix,
        )
        .await?;
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
