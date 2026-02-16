use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
                Self::Major => "\x1b[1;31mMajor\x1b[0m", // bold red
                Self::Minor => "\x1b[1;33mMinor\x1b[0m", // bold yellow
                Self::Patch => "\x1b[1;32mPatch\x1b[0m", // bold green
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(UpdateType::Major, "Major")]
    #[case(UpdateType::Minor, "Minor")]
    #[case(UpdateType::Patch, "Patch")]
    fn test_update_type_display(#[case] update_type: UpdateType, #[case] expected: &str) {
        let display = format!("{}", update_type);
        assert!(display.contains(expected));
    }
}
