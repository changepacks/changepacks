/// Split a version string into borrowed prefix and version parts.
///
/// Returns a `(prefix, version)` tuple of sub-slices of the input: the
/// `prefix` is `Some(&str)` when the input begins with non-digit characters
/// (e.g. `"^"`, `"~"`, `">="`, `"helloworld-"`) and `None` when the input
/// starts with a digit or contains no digits at all (e.g. `"latest"`, `"*"`).
/// Both parts borrow from `version`, so the split is allocation-free — callers
/// that need owned data rebuild `"<prefix><new_version>"` via `format!`. The
/// function is total — every input yields a valid pair — so it does NOT return
/// a `Result`.
pub fn split_version(version: &str) -> (Option<&str>, &str) {
    // Byte-level scan: the predicate `is_ascii_digit()` is a byte check, and
    // ASCII digits are single-byte / cannot appear inside a multi-byte UTF-8
    // sequence, so the byte offset from `.bytes().position(...)` is always a
    // valid char boundary — slicing `version[..pos]` / `version[pos..]` stays
    // panic-free even on multi-byte prefixes like `λ-1.0.0`.
    match version.bytes().position(|b| b.is_ascii_digit()) {
        Some(0) | None => (None, version),
        Some(pos) => (Some(&version[..pos]), &version[pos..]),
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
        assert_eq!(prefix, expected.0);
        assert_eq!(version, expected.1);
    }
}
