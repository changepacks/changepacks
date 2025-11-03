use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub ignore: Vec<String>,

    #[serde(default = "default_base_branch")]
    pub base_branch: String,
}

fn default_base_branch() -> String {
    "main".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ignore: Vec::new(),
            base_branch: default_base_branch(),
        }
    }
}
