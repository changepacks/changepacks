use anyhow::{Context, Result};
use changepacks_core::UpdateType;

/// Calculate the next version based on semver and update type
///
/// # Errors
/// Returns error if the version format is invalid.
pub fn next_version(version: &str, update_type: UpdateType) -> Result<String> {
    let mut version_parts = version.split('.').collect::<Vec<&str>>();

    // Ensure we have exactly 3 parts (major.minor.patch)
    if version_parts.len() != 3 {
        return Err(anyhow::anyhow!("Invalid version format: {version}"));
    }
    // `split_once('+')` returns `None` when there is no `+`, and
    // `Some((base, rest))` otherwise. This replaces the previous
    // `split(...).collect::<Vec<&str>>()` + `len() == 2` guard, which
    // allocated a throwaway `Vec<&str>` on every call on the criterion
    // hot path (`bench_next_version`). Behavior on multi-`+` inputs
    // (e.g. `"1.0.0++"`) is preserved: the old `len() == 2` path also
    // swallowed the trailing `+`, and `split_once` puts it inside `ext`
    // exactly the same way.
    let plus_part = if let Some((base, ext)) = version_parts[2].split_once('+') {
        version_parts[2] = base;
        Some(ext)
    } else {
        None
    };

    let version_index = match update_type {
        UpdateType::Major => 0,
        UpdateType::Minor => 1,
        UpdateType::Patch => 2,
    };

    let version_part = (version_parts[version_index]
        .parse::<usize>()
        .with_context(|| format!("Invalid version: {version}"))?
        + 1)
    .to_string();
    version_parts[version_index] = version_part.as_str();

    // Reset lower version parts to 0
    for part in version_parts.iter_mut().skip(version_index + 1) {
        *part = "0";
    }

    let mut result = version_parts.join(".");
    if let Some(p) = plus_part {
        result.push('+');
        result.push_str(p);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("1.0.0", UpdateType::Major, "2.0.0")]
    #[case("1.0.0", UpdateType::Minor, "1.1.0")]
    #[case("1.0.0", UpdateType::Patch, "1.0.1")]
    #[case("2.5.3", UpdateType::Major, "3.0.0")]
    #[case("2.5.3", UpdateType::Minor, "2.6.0")]
    #[case("2.5.3", UpdateType::Patch, "2.5.4")]
    #[case("0.1.0", UpdateType::Major, "1.0.0")]
    #[case("10.20.30", UpdateType::Major, "11.0.0")]
    #[case("10.20.30", UpdateType::Minor, "10.21.0")]
    #[case("10.20.30", UpdateType::Patch, "10.20.31")]
    #[case("10.20.30+1", UpdateType::Patch, "10.20.31+1")]
    fn test_next_version(
        #[case] version: &str,
        #[case] update_type: UpdateType,
        #[case] expected: &str,
    ) {
        let result = next_version(version, update_type).unwrap();
        assert_eq!(result, expected);
    }

    #[rstest]
    #[case("invalid", UpdateType::Major)]
    #[case("1.2", UpdateType::Minor)]
    #[case("1.2.3.4", UpdateType::Patch)]
    #[case("1.2.wrong", UpdateType::Patch)]
    fn test_next_version_invalid_input(#[case] version: &str, #[case] update_type: UpdateType) {
        let result = next_version(version, update_type);
        assert!(result.is_err());
    }
}
