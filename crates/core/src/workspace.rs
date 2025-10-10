use crate::update_type::UpdateType;
use anyhow::Result;

pub trait Workspace: std::fmt::Debug + Send + Sync {
    fn name(&self) -> Option<&str>;
    fn path(&self) -> &str;
    fn version(&self) -> Option<&str>;
    fn update_version(&mut self, update_type: UpdateType) -> Result<String>;
    fn language(&self) -> &str;
}
