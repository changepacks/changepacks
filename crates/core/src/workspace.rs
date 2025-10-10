use crate::update_type::UpdateType;
use anyhow::Result;

pub trait Workspace: std::fmt::Debug + Send + Sync {
    fn name(&self) -> Option<&str>;
    fn path(&self) -> &str;
    fn version(&self) -> Option<&str>;
    fn update_version(&self, update_type: UpdateType) -> Result<()>;
    fn language(&self) -> &str;
}
