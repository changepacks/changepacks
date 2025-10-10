use crate::update_type::UpdateType;
use anyhow::Result;

pub trait Package: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn path(&self) -> &str;
    fn update_version(&mut self, update_type: UpdateType) -> Result<String>;
    fn language(&self) -> &str;
}
