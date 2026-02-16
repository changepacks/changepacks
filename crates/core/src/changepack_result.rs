use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::update_type::UpdateType;

/// Single changepack log entry for aggregated results.
///
/// Contains the update type and note from a changepack log file.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChangePackResultLog {
    /// Type of version update (Major, Minor, or Patch)
    r#type: UpdateType,
    /// User-provided changelog note
    note: String,
}

impl ChangePackResultLog {
    #[must_use]
    pub const fn new(r#type: UpdateType, note: String) -> Self {
        Self { r#type, note }
    }
}

/// Aggregated version update results for JSON output format.
///
/// Contains all changepack logs applied to a project, current version, next version,
/// and change status.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePackResult {
    /// All changepack logs applied to this project
    logs: Vec<ChangePackResultLog>,
    /// Current version before update
    version: Option<String>,
    /// New version after applying updates
    next_version: Option<String>,
    /// Project name from manifest
    name: Option<String>,
    /// Whether the project has uncommitted changes
    changed: bool,
    /// File path to the project manifest
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
