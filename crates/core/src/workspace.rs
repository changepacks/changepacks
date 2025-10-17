use crate::update_type::UpdateType;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Workspace: std::fmt::Debug + Send + Sync {
    fn name(&self) -> Option<&str>;
    fn path(&self) -> &str;
    fn version(&self) -> Option<&str>;
    async fn update_version(&self, update_type: UpdateType) -> Result<()>;
    fn language(&self) -> &str;
}
