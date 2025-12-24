use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub ignore: Vec<String>,

    #[serde(default = "default_base_branch")]
    pub base_branch: String,

    #[serde(default)]
    pub latest_package: Option<String>,

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
