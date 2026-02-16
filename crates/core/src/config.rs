use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Loaded from `.changepacks/config.json`, controls ignore patterns, base branch, publish commands, and update-on rules.
///
/// Configuration can specify custom publish commands per language or per project path,
/// ignore patterns using globs, and forced update rules for dependent packages.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Glob patterns for files/projects to ignore (e.g., "examples/**")
    #[serde(default)]
    pub ignore: Vec<String>,

    /// Base branch to compare against for change detection (default: "main")
    #[serde(default = "default_base_branch")]
    pub base_branch: String,

    /// Optional path to the default main package for versioning
    #[serde(default)]
    pub latest_package: Option<String>,

    /// Custom publish commands by language key or project path
    #[serde(default)]
    pub publish: HashMap<String, String>,

    /// Dependency rules for forced updates.
    /// Key: glob pattern for trigger packages (e.g., "crates/*")
    /// Value: list of package paths that must be updated when trigger matches
    #[serde(default)]
    pub update_on: HashMap<String, Vec<String>>,
}

fn default_base_branch() -> String {
    "main".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ignore: Vec::new(),
            base_branch: default_base_branch(),
            latest_package: None,
            publish: HashMap::new(),
            update_on: HashMap::new(),
        }
    }
}
