use std::path::Path;

use crate::project::Project;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait ProjectFinder: std::fmt::Debug + Send + Sync {
    fn projects(&self) -> Vec<&Project>;
    fn projects_mut(&mut self) -> Vec<&mut Project>;
    fn project_files(&self) -> &[&str];
    async fn visit(&mut self, path: &Path, relative_path: &Path) -> Result<()>;
    fn check_changed(&mut self, path: &Path) -> Result<()> {
        for project in self.projects_mut() {
            project.check_changed(path)?;
        }
        Ok(())
    }
    async fn test(&self) -> Result<()> {
        Ok(())
    }
}
