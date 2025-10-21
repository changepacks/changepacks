use anyhow::Result;
use changepack_core::update_type::UpdateType;

use crate::next_version;

pub fn display_update(current_version: &str, update_type: UpdateType) -> Result<String> {
    let next_version = next_version(current_version, update_type)?;
    Ok(format!("{} → {}", current_version, next_version))
}
