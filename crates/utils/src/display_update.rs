use anyhow::Result;
use changepacks_core::update_type::UpdateType;

use crate::next_version;

pub fn display_update(current_version: Option<&str>, update_type: UpdateType) -> Result<String> {
    if let Some(current_version) = current_version {
        let next_version = next_version(current_version, update_type)?;
        Ok(format!("v{} → v{}", current_version, next_version))
    } else {
        let next_version = next_version("0.0.0", update_type)?;
        Ok(format!("{} → v{}", "unknown", next_version))
    }
}
