/// Split a version string into prefix and version parts.
///
/// Returns a `(prefix, version)` tuple: the `prefix` is `Some(String)` when
/// the input begins with non-digit characters (e.g. `"^"`, `"~"`, `">="`,
/// `"helloworld-"`) and `None` when the input starts with a digit or contains
/// no digits at all (e.g. `"latest"`, `"*"`). The function is total — every
/// input yields a valid pair — so it does NOT return a `Result`. The
/// previous `Result<(Option<String>, String)>` signature carried an unreached
/// `Err` variant (both match arms already returned `Ok(...)`), so callers
/// wrote `?` / `.unwrap()` against a `None` case that could never happen.
pub fn split_version(version: &str) -> (Option<String>, String) {
    let first_digit_pos = version
        .char_indices()
        .find(|(_, c)| c.is_ascii_digit())
        .map(|(pos, _)| pos);

    match first_digit_pos {
        Some(0) | None => (None, version.to_string()),
        Some(pos) => {
            let prefix = version[..pos].to_string();
            let version_part = version[pos..].to_string();
            (Some(prefix), version_part)
        }
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
        let (prefix, version) = split_version(input);
        assert_eq!(prefix.as_deref(), expected.0);
        assert_eq!(version.as_str(), expected.1);
    }
}
