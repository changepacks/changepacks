use std::path::Path;

use crate::project::Project;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait ProjectFinder: std::fmt::Debug + Send + Sync {
    fn projects(&self) -> Vec<&Project>;
    fn project_files(&self) -> &[&str];
    async fn visit(&mut self, path: &Path) -> Result<()>;
}
