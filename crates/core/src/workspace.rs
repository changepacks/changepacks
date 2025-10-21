use std::path::Path;

use crate::{Language, update_type::UpdateType};
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
        if path.starts_with(self.path().parent().context("Parent not found")?) {
            self.set_changed(true);
        }
        Ok(())
    }

    fn is_changed(&self) -> bool;
    fn set_changed(&mut self, changed: bool);
}
