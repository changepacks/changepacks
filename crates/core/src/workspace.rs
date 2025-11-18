use std::path::Path;

use crate::{Config, Language, Package, update_type::UpdateType};
use anyhow::{Context, Result};
use async_trait::async_trait;

#[async_trait]
pub trait Workspace: std::fmt::Debug + Send + Sync {
    fn name(&self) -> Option<&str>;
    fn path(&self) -> &Path;
    fn relative_path(&self) -> &Path;
    fn version(&self) -> Option<&str>;
    async fn update_version(&self, update_type: UpdateType) -> Result<()>;
    fn language(&self) -> Language;

    // Default implementation for check_changed
    fn check_changed(&mut self, path: &Path) -> Result<()> {
        if self.is_changed() {
            return Ok(());
        }
        if !path.to_string_lossy().contains(".changepacks")
            && path.starts_with(self.path().parent().context("Parent not found")?)
        {
            self.set_changed(true);
        }
        Ok(())
    }

    fn is_changed(&self) -> bool;
    fn set_changed(&mut self, changed: bool);

    /// Get the default publish command for this workspace type
    fn default_publish_command(&self) -> &'static str;

    /// Publish the workspace using the configured command or default
    async fn publish(&self, config: &Config) -> Result<()> {
        let command = self.get_publish_command(config);
        // Get the directory containing the workspace file
        let workspace_dir = self
            .path()
            .parent()
            .context("Workspace directory not found")?;

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C");
            c.arg(command);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c");
            c.arg(command);
            c
        };

        cmd.current_dir(workspace_dir);
        let output = cmd.output().await?;

        if !output.status.success() {
            anyhow::bail!(
                "Publish command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        } else {
            Ok(())
        }
    }

    /// Get the publish command for this workspace, checking config first
    fn get_publish_command(&self, config: &Config) -> String {
        // Check for custom command by relative path
        if let Some(cmd) = config
            .publish
            .get(self.relative_path().to_string_lossy().as_ref())
        {
            return cmd.clone();
        }

        // Check for custom command by language
        let lang_key = match self.language() {
            crate::Language::Node => "node",
            crate::Language::Python => "python",
            crate::Language::Rust => "rust",
            crate::Language::Dart => "dart",
        };
        if let Some(cmd) = config.publish.get(lang_key) {
            return cmd.clone();
        }

        // Use default command
        self.default_publish_command().to_string()
    }

    async fn update_workspace_dependencies(&self, _packages: &[&dyn Package]) -> Result<()> {
        Ok(())
    }
}
