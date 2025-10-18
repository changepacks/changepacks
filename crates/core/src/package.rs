use crate::{Language, update_type::UpdateType};
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Package: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn path(&self) -> &str;
    async fn update_version(&self, update_type: UpdateType) -> Result<()>;
    fn language(&self) -> Language;
}
