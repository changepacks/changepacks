/// Split a version string into borrowed prefix and version parts.
///
/// Returns a `(prefix, version)` tuple of sub-slices of the input: the
/// `prefix` is `Some(&str)` when the input begins with non-digit characters
/// (e.g. `"^"`, `"~"`, `">="`, `"helloworld-"`) and `None` when the input
/// starts with a digit or contains no digits at all (e.g. `"latest"`, `"*"`).
/// Both parts borrow from `version`, so the split is allocation-free — callers
/// that need owned data rebuild `"<prefix><new_version>"` via
/// [`replace_version_keep_prefix`]. The
/// function is total — every input yields a valid pair — so it does NOT return
/// a `Result`.
#[must_use]
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

/// Rebuild a version specifier keeping its range prefix but swapping the
/// numeric tail for `new_version`.
///
/// Splits `spec` via [`split_version`], discards the old numeric tail, and
/// returns an owned `"<prefix><new_version>"` — the exact "preserve the
/// prefix, replace the numeric tail" rebuild [`split_version`]'s own doc
/// names as the intended caller pattern. Centralizing it here keeps that
/// policy in ONE place instead of drifting between hand-rolled `format!`s.
///
/// The result is built at exact capacity rather than through `format!`: both
/// halves are already-measured `&str`s, so the final byte length is known
/// before the first `push_str` and the buffer never reallocates. This is the
/// same preallocation policy `next_version` applies to its own result buffer,
/// and it saves one reallocation per workspace-dependency rewrite.
#[must_use]
pub fn replace_version_keep_prefix(spec: &str, new_version: &str) -> String {
    let (prefix, _) = split_version(spec);
    let prefix = prefix.unwrap_or_default();
    let mut result = String::with_capacity(prefix.len() + new_version.len());
    result.push_str(prefix);
    result.push_str(new_version);
    result
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
    #[case("λ-1.0.0", (Some("λ-"), "1.0.0"))]
    #[case("latest", (None, "latest"))]
    #[case("*", (None, "*"))]
    // Empty input contains no ASCII digit, so `position(...)` yields `None` and
    // the whole (empty) string is returned as the version with no prefix.
    #[case("", (None, ""))]
    fn test_split_version(#[case] input: &str, #[case] expected: (Option<&str>, &str)) {
        let (prefix, version) = split_version(input);
        assert_eq!(prefix, expected.0);
        assert_eq!(version, expected.1);
    }

    #[rstest]
    #[case("1.0.0", "2.0.0", "2.0.0")]
    #[case("^1.0.0", "2.0.0", "^2.0.0")]
    #[case("~1.2.3", "4.5.6", "~4.5.6")]
    #[case(">=1.0.0", "2.0.0", ">=2.0.0")]
    #[case("helloworld-1.0.2", "2.0.0", "helloworld-2.0.0")]
    // A digit-less specifier ("latest", "*", "") has no prefix, so the whole
    // specifier is replaced wholesale by `new_version`. The Cargo
    // workspace-dependency rewrite in `changepacks-rust` relies on exactly this:
    // a non-numeric requirement is overwritten rather than concatenated.
    #[case("latest", "2.0.0", "2.0.0")]
    #[case("*", "2.0.0", "2.0.0")]
    #[case("", "2.0.0", "2.0.0")]
    // A multi-byte prefix is preserved byte-for-byte alongside the new tail.
    #[case("λ-1.0.0", "2.0.0", "λ-2.0.0")]
    fn test_replace_version_keep_prefix(
        #[case] spec: &str,
        #[case] new_version: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(replace_version_keep_prefix(spec, new_version), expected);
    }
}
