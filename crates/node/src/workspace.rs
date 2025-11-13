use anyhow::Result;
use async_trait::async_trait;
use changepacks_core::{Language, UpdateType, Workspace};
use changepacks_utils::{detect_indent, next_version};
use std::path::{Path, PathBuf};
use tokio::fs::{read_to_string, write};

#[derive(Debug)]
pub struct NodeWorkspace {
    path: PathBuf,
    relative_path: PathBuf,
    version: Option<String>,
    name: Option<String>,
    is_changed: bool,
}

impl NodeWorkspace {
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
impl Workspace for NodeWorkspace {
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

        let package_json_raw = read_to_string(Path::new(&self.path)).await?;
        let postfix = if package_json_raw.ends_with("\n") {
            "\n"
        } else {
            ""
        };
        let indent = detect_indent(&package_json_raw);
        let mut package_json: serde_json::Value = serde_json::from_str(&package_json_raw)?;
        package_json["version"] = serde_json::Value::String(next_version.clone());
        let ind = &b" ".repeat(indent);
        let formatter = serde_json::ser::PrettyFormatter::with_indent(ind);
        let mut writer = Vec::new();
        serde_json::Serializer::with_formatter(&mut writer, formatter);
        write(&self.path, String::from_utf8(writer)? + postfix).await?;
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Node
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
