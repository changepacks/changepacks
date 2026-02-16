use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::update_type::UpdateType;

#[derive(Debug, Serialize, Deserialize)]
pub struct ChangePackResultLog {
    r#type: UpdateType,
    note: String,
}

impl ChangePackResultLog {
    #[must_use]
    pub const fn new(r#type: UpdateType, note: String) -> Self {
        Self { r#type, note }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePackResult {
    logs: Vec<ChangePackResultLog>,
    version: Option<String>,
    next_version: Option<String>,
    name: Option<String>,
    changed: bool,
    path: PathBuf,
}

impl ChangePackResult {
    #[must_use]
    pub const fn new(
        logs: Vec<ChangePackResultLog>,
        version: Option<String>,
        next_version: Option<String>,
        name: Option<String>,
        changed: bool,
        path: PathBuf,
    ) -> Self {
        Self {
            logs,
            version,
            next_version,
            name,
            changed,
            path,
        }
    }
}
