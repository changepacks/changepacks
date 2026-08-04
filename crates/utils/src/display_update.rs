use std::borrow::Cow;

use anyhow::Result;
use changepacks_core::UpdateType;

use crate::next_version_or_default;

/// Display the version update as a formatted string
///
/// # Errors
/// Returns error if the next version cannot be calculated.
pub fn display_update(current_version: Option<&str>, update_type: UpdateType) -> Result<String> {
    // Two-branch "reserve `0.0.0` when `None`" prelude consolidated into
    // `next_version_or_default` — the same helper Node/Python/Dart/CSharp
    // already delegate through for their `update_version_from_fields`.
    // The `Some` vs `None` split now only carries the `"v"`-prefix vs
    // `"unknown"` DISPLAY distinction.
    let next_version = next_version_or_default(current_version, update_type)?;
    // `format_version_display` returns a `Cow`, so the `None` ("unknown") case
    // borrows a static literal instead of allocating; `Cow` renders through
    // `Display` exactly like the `String` it replaced.
    let current_display: Cow<'static, str> =
        changepacks_core::format_version_display(current_version);
    Ok(format!("{current_display} → v{next_version}"))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(Some("1.0.0"), UpdateType::Major, "v1.0.0 → v2.0.0")]
    #[case(Some("1.0.0"), UpdateType::Minor, "v1.0.0 → v1.1.0")]
    #[case(Some("1.0.0"), UpdateType::Patch, "v1.0.0 → v1.0.1")]
    #[case(Some("2.5.3"), UpdateType::Major, "v2.5.3 → v3.0.0")]
    #[case(Some("2.5.3"), UpdateType::Minor, "v2.5.3 → v2.6.0")]
    #[case(Some("2.5.3"), UpdateType::Patch, "v2.5.3 → v2.5.4")]
    #[case(Some("0.1.0"), UpdateType::Major, "v0.1.0 → v1.0.0")]
    #[case(Some("10.20.30"), UpdateType::Major, "v10.20.30 → v11.0.0")]
    #[case(Some("10.20.30"), UpdateType::Minor, "v10.20.30 → v10.21.0")]
    #[case(Some("10.20.30"), UpdateType::Patch, "v10.20.30 → v10.20.31")]
    #[case(Some("10.20.30+1"), UpdateType::Patch, "v10.20.30+1 → v10.20.31+1")]
    #[case(None, UpdateType::Major, "unknown → v1.0.0")]
    #[case(None, UpdateType::Minor, "unknown → v0.1.0")]
    #[case(None, UpdateType::Patch, "unknown → v0.0.1")]
    fn test_display_update(
        #[case] current_version: Option<&str>,
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        assert_eq!(
            display_update(current_version, update_type).unwrap(),
            expected
        );
    }

    /// `display_update` renders the `changepacks update` preview straight from
    /// whatever version string the on-disk manifest carries, so a malformed
    /// value reaches it unfiltered. Pin that the `?` at the
    /// `next_version_or_default` call propagates instead of silently falling
    /// back to a placeholder preview, and that the flattened chain still names
    /// the offending text so the operator can find the bad manifest.
    #[test]
    fn test_display_update_rejects_malformed_version() {
        let error = display_update(Some("abc"), UpdateType::Patch)
            .expect_err("a non-semver current version must not render a preview");
        let chain = error
            .chain()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ");
        assert!(
            chain.contains("abc"),
            "error chain must name the offending version, got: {chain}"
        );
    }
}
