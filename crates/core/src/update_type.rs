use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpdateType {
    Major = 0,
    Minor = 1,
    Patch = 2,
}

impl Display for UpdateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                UpdateType::Major => "\x1b[1;31mMajor\x1b[0m", // bold red
                UpdateType::Minor => "\x1b[1;33mMinor\x1b[0m", // bold yellow
                UpdateType::Patch => "\x1b[1;32mPatch\x1b[0m", // bold green
            }
        )
    }
}
