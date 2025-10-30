use std::path::Path;

use crate::{Language, update_type::UpdateType};
use anyhow::{Context, Result};
use async_trait::async_trait;

#[async_trait]
pub trait Package: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn path(&self) -> &Path;
    fn relative_path(&self) -> &Path;
    async fn update_version(&self, update_type: UpdateType) -> Result<()>;
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
}
