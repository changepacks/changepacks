use std::path::Path;

use crate::{Config, Language, update_type::UpdateType};
use anyhow::{Context, Result};
use async_trait::async_trait;

#[async_trait]
pub trait Package: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn path(&self) -> &Path;
    fn relative_path(&self) -> &Path;
    async fn update_version(&mut self, update_type: UpdateType) -> Result<()>;
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
    fn language(&self) -> Language;
    fn set_changed(&mut self, changed: bool);

    /// Get the default publish command for this package type
    fn default_publish_command(&self) -> &'static str;

    /// Publish the package using the configured command or default
    async fn publish(&self, config: &Config) -> Result<()> {
        let command = self.get_publish_command(config);
        // Get the directory containing the package file
        let package_dir = self
            .path()
            .parent()
            .context("Package directory not found")?;

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

        cmd.current_dir(package_dir);
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

    /// Get the publish command for this package, checking config first
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
}
