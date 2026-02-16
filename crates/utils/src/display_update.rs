use anyhow::Result;
use changepacks_core::UpdateType;

use crate::next_version;

pub fn display_update(current_version: Option<&str>, update_type: UpdateType) -> Result<String> {
    if let Some(current_version) = current_version {
        let next_version = next_version(current_version, update_type)?;
        Ok(format!("v{current_version} → v{next_version}"))
    } else {
        let next_version = next_version("0.0.0", update_type)?;
        Ok(format!("{} → v{}", "unknown", next_version))
    }
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
}
