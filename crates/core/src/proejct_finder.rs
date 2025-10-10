use std::path::Path;

use anyhow::Result;

use crate::project::Project;

pub trait ProjectFinder: Send + Sync {
    fn projects(&self) -> Vec<&Project>;
    fn project_files(&self) -> &[&str];
    fn visit(&mut self, path: &Path) -> Result<()>;
}
