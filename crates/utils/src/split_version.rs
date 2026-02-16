use anyhow::Result;

/// Split a version string into prefix and version parts
///
/// # Errors
/// Returns error if splitting the version fails (currently never fails).
pub fn split_version(version: &str) -> Result<(Option<String>, String)> {
    let first_digit_pos = version
        .char_indices()
        .find(|(_, c)| c.is_ascii_digit())
        .map(|(pos, _)| pos);

    match first_digit_pos {
        Some(0) => Ok((None, version.to_string())),
        Some(pos) => {
            let prefix = version[..pos].to_string();
            let version_part = version[pos..].to_string();
            Ok((Some(prefix), version_part))
        }
        None => Ok((None, version.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rstest::rstest;

    #[rstest]
    #[case("1.0.0", (None, "1.0.0"))]
    #[case("^1.0.0", (Some("^"), "1.0.0"))]
    #[case("~1.0.0", (Some("~"), "1.0.0"))]
    #[case("1.0.0-alpha.1", (None, "1.0.0-alpha.1"))]
    #[case("1.0.0-alpha.1+build1", (None, "1.0.0-alpha.1+build1"))]
    #[case(">=1.0.0+build1", (Some(">="), "1.0.0+build1"))]
    #[case("helloworld-1.0.2", (Some("helloworld-"), "1.0.2"))]
    #[case("latest", (None, "latest"))]
    #[case("*", (None, "*"))]
    fn test_split_version(#[case] input: &str, #[case] expected: (Option<&str>, &str)) {
        let (prefix, version) = split_version(input).unwrap();
        assert_eq!(prefix.as_deref(), expected.0);
        assert_eq!(version.as_str(), expected.1);
    }
}
