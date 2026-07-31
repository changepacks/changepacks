use colored::Colorize;
use std::fmt::Display;

use serde::{Deserialize, Serialize};

/// Semantic versioning bump types following semver conventions.
///
/// Determines how the version number increments: major (breaking), minor (features), or patch (fixes).
///
/// The derived `Ord` compares the explicit discriminants below, so `Major < Minor < Patch`
/// orders the variants most-severe-first. `gen_update_map` relies on exactly that: when
/// several changepack logs name the same project it keeps the smaller value, i.e. the most
/// severe bump. Changing the discriminants would silently downgrade merged bumps, so the
/// ordering test below must fail rather than let that through.
///
/// The variant names are also the serialized wire form of
/// `.changepacks/changepack_log_*.json`, so renaming a variant breaks existing logs.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpdateType {
    /// Breaking changes: increments X.0.0
    Major = 0,
    /// New features, backward-compatible: increments 0.X.0
    Minor = 1,
    /// Bug fixes, backward-compatible: increments 0.0.X
    Patch = 2,
}

impl Display for UpdateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Major => "Major".bright_red().bold(),
                Self::Minor => "Minor".bright_yellow().bold(),
                Self::Patch => "Patch".bright_green().bold(),
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
        let display = update_type.to_string();
        assert!(display.contains(expected));
    }

    /// `gen_update_map` merges several changepack logs that name the same project with
    /// `if ret.0 > *update_type { ret.0 = *update_type; }`, which keeps the most severe
    /// bump only because the derived `Ord` puts `Major` before `Minor` before `Patch`.
    /// Changing the explicit discriminants must fail here rather than silently downgrade
    /// a merge.
    #[test]
    fn test_update_type_severity_order_is_the_merge_contract() {
        assert!(UpdateType::Major < UpdateType::Minor);
        assert!(UpdateType::Minor < UpdateType::Patch);

        let mut declared = [UpdateType::Patch, UpdateType::Major, UpdateType::Minor];
        declared.sort();
        assert_eq!(
            declared,
            [UpdateType::Major, UpdateType::Minor, UpdateType::Patch],
            "sorting must order the most severe bump first"
        );

        // The merge itself: the minimum of a mixed set is the most severe bump.
        assert_eq!(declared.iter().copied().min(), Some(UpdateType::Major));
    }

    /// The variant names are the on-disk form inside `.changepacks/changepack_log_*.json`,
    /// so the wire form must stay exactly the capitalized variant name.
    #[rstest]
    #[case(UpdateType::Major, "\"Major\"")]
    #[case(UpdateType::Minor, "\"Minor\"")]
    #[case(UpdateType::Patch, "\"Patch\"")]
    fn test_update_type_serde_wire_form_is_stable(
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        let serialized = serde_json::to_string(&update_type).expect("serialization must succeed");
        assert_eq!(serialized, expected);

        let round_tripped: UpdateType =
            serde_json::from_str(&serialized).expect("deserialization must succeed");
        assert_eq!(round_tripped, update_type);
    }

    #[test]
    fn test_update_type_serde_rejects_lowercase_variant_name() {
        let parsed = serde_json::from_str::<UpdateType>("\"major\"");
        assert!(
            parsed.is_err(),
            "lowercase variant names are not part of the wire form"
        );
    }
}
