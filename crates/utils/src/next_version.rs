use anyhow::{Context, Result};
use changepacks_core::update_type::UpdateType;

pub fn next_version(version: &str, update_type: UpdateType) -> Result<String> {
    let mut version_parts = version.split(".").collect::<Vec<&str>>();

    // Ensure we have exactly 3 parts (major.minor.patch)
    if version_parts.len() != 3 {
        return Err(anyhow::anyhow!("Invalid version format: {}", version));
    }
    let plus_split = version_parts[2].split("+").collect::<Vec<&str>>();
    let plus_part = if plus_split.len() == 2 {
        version_parts[2] = plus_split[0];
        Some(plus_split[1])
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
        .context(format!("Invalid version: {}", version))?
        + 1)
    .to_string();
    version_parts[version_index] = version_part.as_str();

    // Reset lower version parts to 0
    for part in version_parts.iter_mut().skip(version_index + 1) {
        *part = "0";
    }

    Ok(format!(
        "{}{}",
        version_parts.join("."),
        plus_part.map(|p| format!("+{}", p)).unwrap_or_default()
    ))
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
